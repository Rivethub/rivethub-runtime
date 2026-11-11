use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use capture::sanitize::KeyError;
use capture::sanitize::key::PrivateFile;
use chrono::DateTime;
use chrono::Utc;
use clap::Subcommand;
use config::Environment;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::Report;

const CLIENT_ID: &str = "rivethub-cli";
const SCOPE: &str = "drops:write";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const REFRESH_GRANT: &str = "refresh_token";
const DEVICE_PATH: &str = "api/cli/device/code";
const TOKEN_PATH: &str = "api/cli/token";
const REVOKE_PATH: &str = "api/cli/revoke";
const STORE_FILE: &str = "cloud.json";
const STORE_NAME: &str = "cloud login";
const STORE_CAP: u64 = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SLOW_DOWN: Duration = Duration::from_secs(5);
const REFRESH_MARGIN: chrono::TimeDelta = chrono::TimeDelta::seconds(60);

#[derive(Debug, Subcommand)]
pub enum CloudCommand {
    #[command(about = "Sign this machine in to RivetHub Cloud in your browser")]
    Login,
    #[command(about = "Revoke this machine's RivetHub Cloud login and forget it")]
    Logout,
    #[command(about = "Show which RivetHub Cloud this machine is signed in to")]
    Status,
}

#[derive(Debug, Error)]
pub enum CloudError {
    #[error(
        "memory.share.cloud.url is not set; add the RivetHub Cloud address to the config, for example https://rivetos.cloud"
    )]
    Unconfigured,
    #[error("memory.share.cloud.url {0:?} is not a URL")]
    Url(String),
    #[error("memory.share.cloud.url {0} must use https unless it is a loopback address")]
    Insecure(Url),
    #[error("this machine is not signed in to RivetHub Cloud; run rh cloud login")]
    LoggedOut,
    #[error("this machine is signed in to {stored}, not {configured}; run rh cloud login")]
    OtherCloud { stored: String, configured: String },
    #[error("the RivetHub Cloud login expired or was revoked; run rh cloud login again ({0})")]
    Expired(OauthCode),
    #[error("the browser approval was {0}; run rh cloud login to try again")]
    Denied(&'static str),
    #[error("RivetHub Cloud answered {status}: {body}")]
    Status { status: u16, body: String },
    #[error("RivetHub Cloud could not be reached: {0}")]
    Http(#[from] reqwest::Error),
    #[error("the RivetHub Cloud login file is unreadable: {0}")]
    Store(String),
    #[error(transparent)]
    File(#[from] KeyError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("login was cancelled")]
    Cancelled,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tokens {
    url: String,
    access_token: String,
    refresh_token: String,
    expires_at: DateTime<Utc>,
    scope: String,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 140: cloud tokens never reach a log; Debug prints only where they work"
    )
)]
impl std::fmt::Debug for Tokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Tokens({})", self.url)
    }
}

#[derive(Deserialize)]
struct Issued {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    scope: String,
}

#[derive(Deserialize)]
struct Device {
    #[serde(rename = "device_code")]
    code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct OauthError {
    error: OauthCode,
    interval: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum OauthCode {
    AuthorizationPending,
    SlowDown,
    AccessDenied,
    ExpiredToken,
    InvalidGrant,
    #[serde(other)]
    Other,
}

#[derive(Serialize)]
struct DeviceRequest<'a> {
    client_id: &'a str,
    scope: &'a str,
    client_name: &'a str,
}

#[derive(Serialize)]
struct Grant<'a> {
    #[serde(rename = "grant_type")]
    kind: &'a str,
    client_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<&'a str>,
}

#[derive(Serialize)]
struct Revoke<'a> {
    client_id: &'a str,
    token: &'a str,
}

#[derive(Debug, Clone)]
pub(crate) struct Cloud {
    url: Url,
    client: reqwest::Client,
    store: PathBuf,
}

pub(crate) fn client(url: &Url, timeout: Duration) -> Result<reqwest::Client, CloudError> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .https_only(!is_loopback(url))
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .build()?)
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name == "localhost",
        None => false,
    }
}

impl Cloud {
    pub(crate) async fn from_env(env: &Environment) -> Result<Self, crate::Error> {
        let loaded = crate::db::load(env, None).await?;
        let section = loaded
            .config
            .share_cloud()
            .ok_or(CloudError::Unconfigured)?;
        let text = section.url.trim();
        let mut url = Url::parse(text).map_err(|_| CloudError::Url(text.to_owned()))?;
        if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback(&url)) {
            return Err(CloudError::Insecure(url).into());
        }
        if !url.path().ends_with('/') {
            url.set_path(&format!("{}/", url.path()));
        }
        Ok(Self {
            client: client(&url, REQUEST_TIMEOUT)?,
            url,
            store: capture::share::state_home(env)?
                .join(capture::sanitize::key::DIR_NAME)
                .join(STORE_FILE),
        })
    }

    pub(crate) fn endpoint(&self, path: &str) -> Result<Url, CloudError> {
        self.url
            .join(path)
            .map_err(|_| CloudError::Url(self.url.to_string()))
    }

    pub(crate) fn origin(&self) -> &Url {
        &self.url
    }

    fn stored(&self) -> Result<Option<Tokens>, CloudError> {
        let bytes = match PrivateFile::new(&self.store, STORE_NAME, STORE_CAP).read() {
            Ok(bytes) => bytes,
            Err(KeyError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| CloudError::Store(error.to_string()))
    }

    fn save(&self, tokens: &Tokens) -> Result<(), CloudError> {
        let parent = self
            .store
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut partial = tempfile::Builder::new()
            .prefix(".cloud-")
            .tempfile_in(parent)?;
        let text =
            serde_json::to_vec(tokens).map_err(|error| CloudError::Store(error.to_string()))?;
        partial.write_all(&text)?;
        partial.as_file().sync_all()?;
        partial
            .persist(&self.store)
            .map_err(|error| CloudError::Io(error.error))?;
        Ok(())
    }

    fn mine(&self, tokens: Tokens) -> Result<Tokens, CloudError> {
        if tokens.url == self.url.as_str() {
            Ok(tokens)
        } else {
            Err(CloudError::OtherCloud {
                stored: tokens.url,
                configured: self.url.to_string(),
            })
        }
    }

    pub(crate) async fn access_token(&self, force_refresh: bool) -> Result<String, CloudError> {
        let tokens = self.mine(self.stored()?.ok_or(CloudError::LoggedOut)?)?;
        if !force_refresh && tokens.expires_at - REFRESH_MARGIN > Utc::now() {
            return Ok(tokens.access_token);
        }
        let response = self
            .client
            .post(self.endpoint(TOKEN_PATH)?)
            .json(&Grant {
                kind: REFRESH_GRANT,
                client_id: CLIENT_ID,
                device_code: None,
                refresh_token: Some(&tokens.refresh_token),
            })
            .send()
            .await?;
        if response.status().is_client_error() {
            let status = response.status().as_u16();
            let body = response.text().await?;
            let error: OauthError =
                serde_json::from_str(&body).map_err(|_| CloudError::Status { status, body })?;
            return Err(CloudError::Expired(error.error));
        }
        let issued: Issued = checked(response).await?.json().await?;
        let renewed = self.tokens(issued);
        self.save(&renewed)?;
        Ok(renewed.access_token)
    }

    fn tokens(&self, issued: Issued) -> Tokens {
        Tokens {
            url: self.url.to_string(),
            access_token: issued.access_token,
            refresh_token: issued.refresh_token,
            expires_at: Utc::now() + chrono::TimeDelta::seconds(issued.expires_in),
            scope: issued.scope,
        }
    }

    async fn login(
        &self,
        client_name: &str,
        shutdown: &CancellationToken,
    ) -> Result<Report, CloudError> {
        let device: Device = checked(
            self.client
                .post(self.endpoint(DEVICE_PATH)?)
                .json(&DeviceRequest {
                    client_id: CLIENT_ID,
                    scope: SCOPE,
                    client_name,
                })
                .send()
                .await?,
        )
        .await?
        .json()
        .await?;
        let link = device
            .verification_uri_complete
            .as_deref()
            .unwrap_or(&device.verification_uri);
        writeln!(
            std::io::stderr(),
            "Open {link} and approve the code {} (it expires in {} minutes).",
            device.user_code,
            device.expires_in.div_ceil(60)
        )?;
        let mut interval = Duration::from_secs(device.interval.unwrap_or(5).max(1));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(device.expires_in);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return Err(CloudError::Cancelled),
                () = tokio::time::sleep(interval) => {}
            }
            if tokio::time::Instant::now() > deadline {
                return Err(CloudError::Denied("not given before the code expired"));
            }
            let response = self
                .client
                .post(self.endpoint(TOKEN_PATH)?)
                .json(&Grant {
                    kind: DEVICE_GRANT,
                    client_id: CLIENT_ID,
                    device_code: Some(&device.code),
                    refresh_token: None,
                })
                .send()
                .await?;
            if response.status().is_success() {
                let tokens = self.tokens(response.json().await?);
                self.save(&tokens)?;
                return Ok(Report::success(format!(
                    "signed in to {} (scope {}); the login lasts until rh cloud logout or a revoke in the dashboard\n",
                    self.url, tokens.scope
                )));
            }
            let status = response.status().as_u16();
            let body = response.text().await?;
            let error: OauthError =
                serde_json::from_str(&body).map_err(|_| CloudError::Status {
                    status,
                    body: body.clone(),
                })?;
            match error.error {
                OauthCode::AuthorizationPending => {}
                OauthCode::SlowDown => {
                    interval = error
                        .interval
                        .map_or(interval + SLOW_DOWN, Duration::from_secs);
                }
                OauthCode::AccessDenied => {
                    return Err(CloudError::Denied("denied in the browser"));
                }
                OauthCode::ExpiredToken => {
                    return Err(CloudError::Denied("not given before the code expired"));
                }
                OauthCode::InvalidGrant | OauthCode::Other => {
                    return Err(CloudError::Status { status, body });
                }
            }
        }
    }

    async fn logout(&self) -> Result<Report, CloudError> {
        let Some(tokens) = self.stored()? else {
            return Ok(Report::success(
                "this machine is not signed in to RivetHub Cloud\n".to_owned(),
            ));
        };
        let revoked = if tokens.url == self.url.as_str() {
            checked(
                self.client
                    .post(self.endpoint(REVOKE_PATH)?)
                    .json(&Revoke {
                        client_id: CLIENT_ID,
                        token: &tokens.refresh_token,
                    })
                    .send()
                    .await?,
            )
            .await
            .map(|_| ())
        } else {
            Ok(())
        };
        std::fs::remove_file(&self.store)?;
        revoked?;
        Ok(Report::success(format!(
            "signed out of {} and revoked this machine's login\n",
            tokens.url
        )))
    }

    fn status(&self) -> Result<Report, CloudError> {
        let Some(tokens) = self.stored()? else {
            return Ok(Report::success(format!(
                "not signed in; rh cloud login signs in to {}\n",
                self.url
            )));
        };
        let tokens = self.mine(tokens)?;
        Ok(Report::success(format!(
            "signed in to {} (scope {}); the access token renews itself, next at {}\n",
            tokens.url,
            tokens.scope,
            tokens.expires_at.format("%Y-%m-%d %H:%M UTC")
        )))
    }
}

pub(crate) async fn checked(response: reqwest::Response) -> Result<reqwest::Response, CloudError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let body = response.text().await?;
    Err(CloudError::Status {
        status,
        body: body.chars().take(200).collect(),
    })
}

pub async fn run(
    command: CloudCommand,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Report, crate::Error> {
    let cloud = Cloud::from_env(env).await?;
    Ok(match command {
        CloudCommand::Login => {
            let name = format!("RivetHub CLI on {}", host_name()?);
            cloud.login(&name, shutdown).await?
        }
        CloudCommand::Logout => cloud.logout().await?,
        CloudCommand::Status => cloud.status()?,
    })
}

fn host_name() -> Result<String, crate::Error> {
    let name = nix::unistd::gethostname()
        .map_err(|_| crate::Error::HostName)?
        .into_string()
        .map_err(|_| crate::Error::HostName)?;
    Ok(name.trim().to_owned())
}
