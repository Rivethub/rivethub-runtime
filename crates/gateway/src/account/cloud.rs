use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;
use wire::gateway::account::ReleaseChannel;

use super::AccountError;
use super::telemetry::{CrashReport, UsageReport};
use super::update::{CloudOs, ReleaseManifest};

const RESPONSE_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct Cloud {
    base: Url,
    http: reqwest::Client,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartBody<'a> {
    email: &'a str,
    install_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Started {
    pub request_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PollBody<'a> {
    request_id: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Granted {
    install_token: String,
    email: String,
}

#[derive(Debug)]
pub enum Poll {
    Pending,
    Expired,
    SignedIn { token: String, email: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Accepted,
    Rejected,
}

impl Cloud {
    pub fn new(base: Url) -> Result<Self, AccountError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("rh/", env!("CARGO_PKG_VERSION")))
            .https_only(base.scheme() == "https")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .no_proxy()
            .build()?;
        Ok(Self { base, http })
    }

    fn endpoint(&self, path: &str) -> Result<Url, AccountError> {
        Ok(self.base.join(path)?)
    }

    pub async fn start(&self, email: &str, install_id: Uuid) -> Result<Started, AccountError> {
        let response = self
            .http
            .post(self.endpoint("v1/auth/start")?)
            .json(&StartBody { email, install_id })
            .send()
            .await?;
        match response.status() {
            StatusCode::OK => json(response).await,
            StatusCode::UNPROCESSABLE_ENTITY => Err(AccountError::Email),
            status => Err(refused(status)),
        }
    }

    pub async fn poll(&self, request_id: &str) -> Result<Poll, AccountError> {
        let response = self
            .http
            .post(self.endpoint("v1/auth/poll")?)
            .json(&PollBody { request_id })
            .send()
            .await?;
        match response.status() {
            StatusCode::OK => {
                let granted: Granted = json(response).await?;
                Ok(Poll::SignedIn {
                    token: granted.install_token,
                    email: granted.email,
                })
            }
            StatusCode::ACCEPTED => Ok(Poll::Pending),
            StatusCode::GONE => Ok(Poll::Expired),
            status => Err(refused(status)),
        }
    }

    pub async fn sign_out(&self, token: &str) -> Result<(), AccountError> {
        self.authorized(reqwest::Method::POST, "v1/auth/signout", token)
            .await
    }

    pub async fn delete(&self, token: &str) -> Result<(), AccountError> {
        self.authorized(reqwest::Method::DELETE, "v1/account", token)
            .await
    }

    async fn authorized(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &str,
    ) -> Result<(), AccountError> {
        let response = self
            .http
            .request(method, self.endpoint(path)?)
            .bearer_auth(token)
            .send()
            .await?;
        match response.status() {
            StatusCode::OK => Ok(()),
            status => Err(refused(status)),
        }
    }

    pub async fn crash(&self, report: &CrashReport) -> Result<Delivery, AccountError> {
        self.telemetry("v1/telemetry/crash", report).await
    }

    pub async fn usage(&self, report: &UsageReport) -> Result<Delivery, AccountError> {
        self.telemetry("v1/telemetry/usage", report).await
    }

    async fn telemetry(
        &self,
        path: &str,
        report: &impl Serialize,
    ) -> Result<Delivery, AccountError> {
        let response = self
            .http
            .post(self.endpoint(path)?)
            .json(report)
            .send()
            .await?;
        match response.status() {
            StatusCode::ACCEPTED => Ok(Delivery::Accepted),
            StatusCode::UNPROCESSABLE_ENTITY | StatusCode::PAYLOAD_TOO_LARGE => {
                Ok(Delivery::Rejected)
            }
            status => Err(refused(status)),
        }
    }

    pub async fn latest(
        &self,
        token: &str,
        channel: ReleaseChannel,
        os: CloudOs,
    ) -> Result<Option<ReleaseManifest>, AccountError> {
        let mut url = self.endpoint("v1/updates/latest")?;
        url.query_pairs_mut()
            .append_pair("channel", &channel.to_string())
            .append_pair("os", &os.to_string());
        let response = self.http.get(url).bearer_auth(token).send().await?;
        match response.status() {
            StatusCode::OK => Ok(Some(json(response).await?)),
            StatusCode::NO_CONTENT => Ok(None),
            status => Err(refused(status)),
        }
    }

    pub async fn download(&self, url: Url, limit: usize) -> Result<Vec<u8>, AccountError> {
        if self.base.scheme() == "https" && url.scheme() != "https" {
            return Err(AccountError::Manifest);
        }
        let mut response = self.http.get(url).send().await?.error_for_status()?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err(AccountError::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

async fn json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, AccountError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > RESPONSE_LIMIT.saturating_sub(bytes.len()) {
            return Err(AccountError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| AccountError::CloudReply)
}

fn refused(status: StatusCode) -> AccountError {
    match status {
        StatusCode::UNAUTHORIZED => AccountError::SignedOutElsewhere,
        StatusCode::TOO_MANY_REQUESTS => AccountError::RateLimited,
        status => AccountError::CloudStatus(status.as_u16()),
    }
}
