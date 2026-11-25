use std::time::Duration;

use capture::seal::DropId;
use futures::StreamExt as _;
use reqwest::StatusCode;
use reqwest::header::CONTENT_TYPE;
use tokio::io::AsyncWriteExt as _;
use wire::gateway::drops::Uploaded;
use wire::gateway::users::TokenScope;

use capture::seal::ShareServer;

use crate::share_server::Server;
use crate::share_server::ServerError;
use crate::share_server::ShareServers;

const DROPS_PATH: &str = "api/drops";
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(300);
pub(super) const MAX_DROP_BYTES: usize = 64 << 20;
const SERVER_RETRIES: u32 = 2;
const RETRY_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub(super) struct DropBox {
    server: Server,
}

#[derive(Debug, thiserror::Error)]
pub enum DropError {
    #[error(transparent)]
    Server(#[from] ServerError),
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
    pub(super) async fn on(servers: &ShareServers, host: &ShareServer) -> Result<Self, DropError> {
        Ok(Self {
            server: servers.connect(host, TRANSFER_TIMEOUT).await?,
        })
    }

    pub(super) async fn on_this_node(
        env: &config::Environment,
        servers: &ShareServers,
    ) -> Result<Self, DropError> {
        let config = crate::db::load(env, None)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .config;
        let endpoint = crate::status::gateway_with(
            env,
            &config,
            TRANSFER_TIMEOUT,
            crate::status::Redirects::Refuse,
            None,
            crate::gateway_target::Locate::Discover,
        )
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self {
            server: servers.on_this_node(endpoint)?,
        })
    }

    pub(super) const fn host(&self) -> &ShareServer {
        self.server.host()
    }

    pub(super) async fn upload(
        &self,
        ciphertext: bytes::Bytes,
    ) -> Result<(DropId, Uploaded), DropError> {
        let url = self.server.endpoint(DROPS_PATH)?;
        let mut rejected: Option<String> = None;
        loop {
            let token = self.server.access_token(rejected.as_deref()).await?;
            let response = self
                .server
                .client()
                .post(url.clone())
                .bearer_auth(&token)
                .header(CONTENT_TYPE, "application/octet-stream")
                .body(ciphertext.clone())
                .send()
                .await
                .map_err(ServerError::from)?;
            if response.status() == StatusCode::UNAUTHORIZED && rejected.is_none() {
                rejected = Some(token);
                continue;
            }
            if response.status() == StatusCode::NOT_FOUND {
                return Err(ServerError::DropsOff(self.host().clone()).into());
            }
            if !response.status().is_success() {
                return Err(self
                    .server
                    .refused(response.status(), TokenScope::DropsWrite)
                    .into());
            }
            let uploaded: Uploaded = response.json().await.map_err(ServerError::from)?;
            let id = uploaded
                .id
                .parse()
                .map_err(|_| DropError::BadId(uploaded.id.clone()))?;
            return Ok((id, uploaded));
        }
    }

    pub(super) async fn download(&self, id: &DropId, sink: std::fs::File) -> Result<(), DropError> {
        let url = self.server.endpoint(&format!("{DROPS_PATH}/{id}"))?;
        let mut attempts: u32 = 0;
        let response = loop {
            let response = self
                .server
                .client()
                .get(url.clone())
                .send()
                .await
                .map_err(ServerError::from)?;
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
        let response = crate::share_server::checked(response)?;
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
