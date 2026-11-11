use std::time::Duration;

use capture::seal::DropId;
use chrono::DateTime;
use chrono::Utc;
use config::Environment;
use futures::StreamExt as _;
use reqwest::StatusCode;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use tokio::io::AsyncWriteExt as _;

use crate::cloud::Cloud;
use crate::cloud::CloudError;

const DROPS_PATH: &str = "api/drops";
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(300);
pub(super) const MAX_DROP_BYTES: usize = 64 << 20;
const SERVER_RETRIES: u32 = 2;
const RETRY_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub(super) struct DropBox {
    cloud: Cloud,
    transfer: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub(super) struct Uploaded {
    pub(super) id: String,
    pub(super) expires_at: DateTime<Utc>,
    pub(super) bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum DropError {
    #[error(transparent)]
    Cloud(#[from] CloudError),
    #[error(
        "the drop box has no bundle {0}: it was already received, revoked, or it expired (drops last 7 days)"
    )]
    Gone(DropId),
    #[error("the drop box sent more than {MAX_DROP_BYTES} bytes, so the download was dropped")]
    TooLarge,
    #[error("the drop box answered with an id that is not a drop id: {0:?}")]
    BadId(String),
    #[error(
        "the download of bundle {0} stopped before the end; a drop burns once its download starts, so ask the sender to share it again"
    )]
    Burned(DropId),
    #[error(
        "the drop box failed while serving bundle {0} and the drop is now gone; ask the sender to share it again"
    )]
    FailedMidway(DropId),
    #[error("the inbox could not hold the download: {0}")]
    Io(#[from] std::io::Error),
}

impl DropBox {
    pub(super) async fn from_env(env: &Environment) -> Result<Self, crate::Error> {
        let cloud = Cloud::from_env(env).await?;
        let transfer = crate::cloud::client(cloud.origin(), TRANSFER_TIMEOUT)?;
        Ok(Self { cloud, transfer })
    }

    pub(super) async fn upload(
        &self,
        ciphertext: bytes::Bytes,
    ) -> Result<(DropId, Uploaded), DropError> {
        let url = self.cloud.endpoint(DROPS_PATH)?;
        let mut rejected: Option<String> = None;
        loop {
            let token = self.cloud.access_token(rejected.as_deref()).await?;
            let response = self
                .transfer
                .post(url.clone())
                .bearer_auth(&token)
                .header(CONTENT_TYPE, "application/octet-stream")
                .body(ciphertext.clone())
                .send()
                .await
                .map_err(CloudError::from)?;
            if response.status() == StatusCode::UNAUTHORIZED && rejected.is_none() {
                rejected = Some(token);
                continue;
            }
            let uploaded: Uploaded = crate::cloud::checked(response)
                .await?
                .json()
                .await
                .map_err(CloudError::from)?;
            let id = uploaded
                .id
                .parse()
                .map_err(|_| DropError::BadId(uploaded.id.clone()))?;
            return Ok((id, uploaded));
        }
    }

    pub(super) async fn download(&self, id: &DropId, sink: std::fs::File) -> Result<(), DropError> {
        let url = self.cloud.endpoint(&format!("{DROPS_PATH}/{id}"))?;
        let mut attempts: u32 = 0;
        let response = loop {
            let response = self
                .transfer
                .get(url.clone())
                .send()
                .await
                .map_err(CloudError::from)?;
            if response.status().is_server_error() && attempts < SERVER_RETRIES {
                attempts += 1;
                tokio::time::sleep(RETRY_DELAY * attempts).await;
                continue;
            }
            break response;
        };
        if response.status() == StatusCode::NOT_FOUND && attempts > 0 {
            return Err(DropError::FailedMidway(id.clone()));
        }
        if response.status() == StatusCode::NOT_FOUND {
            return Err(DropError::Gone(id.clone()));
        }
        let response = crate::cloud::checked(response).await?;
        let declared = response.content_length();
        if declared.is_some_and(|length| length > MAX_DROP_BYTES as u64) {
            return Err(DropError::TooLarge);
        }
        let mut file = tokio::fs::File::from_std(sink);
        let mut received: u64 = 0;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| DropError::Burned(id.clone()))?;
            received += chunk.len() as u64;
            if received > MAX_DROP_BYTES as u64 {
                return Err(DropError::TooLarge);
            }
            file.write_all(&chunk).await?;
        }
        if declared.is_some_and(|length| length != received) {
            return Err(DropError::Burned(id.clone()));
        }
        file.flush().await?;
        Ok(())
    }
}
