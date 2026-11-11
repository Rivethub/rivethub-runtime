use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use capture::sanitize::KeyError;
use capture::sanitize::key::PrivateFile;
use capture::seal::CodeError;
use capture::seal::ShareServer;
use capture::share_policy::{BadAllowedServer, SharePolicy, ShareRefused};
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
const STORE_FILE: &str = "share-logins.json";
const LOCK_FILE: &str = "share-logins.json.lock";
const STORE_NAME: &str = "share server logins";
const STORE_CAP: u64 = 256 * 1024;
const CA_CAP: u64 = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SLOW_DOWN: Duration = Duration::from_secs(5);
const REFRESH_MARGIN: chrono::TimeDelta = chrono::TimeDelta::seconds(60);

#[derive(Debug, Subcommand)]
pub enum ShareCommand {
    #[command(
        about = "Sign this machine in to a share server (default rivethub.cloud) in your browser"
    )]
    Login {
        #[arg(
            help = "Share server host, such as team.example.com (default: memory.share.server.url, else rivethub.cloud)"
        )]
        server: Option<String>,
    },
    #[command(about = "Revoke this machine's login on a share server and forget it")]
    Logout {
        #[arg(help = "Share server host (default: memory.share.server.url, else rivethub.cloud)")]
        server: Option<String>,
    },
    #[command(about = "Show the share servers this machine is signed in to")]
    Status,
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    Server(#[from] CodeError),
    #[error(transparent)]
    AllowedServer(#[from] BadAllowedServer),
    #[error(transparent)]
    Refused(#[from] ShareRefused),
    #[error("this machine is not signed in to {0}; run rh share login {0}")]
    LoggedOut(ShareServer),
    #[error("the login on {server} expired or was revoked; run rh share login {server} ({code})")]
    Expired {
        server: ShareServer,
        code: OauthCode,
    },
    #[error("the browser approval was {0}; run rh share login to try again")]
    Denied(&'static str),
    #[error("the share server answered {status}: {body}")]
    Status { status: u16, body: String },
    #[error("the share server could not be reached: {0}")]
    Http(#[from] reqwest::Error),
    #[error("the share login file is unreadable: {0}")]
    Store(String),
    #[error("the CA file {path} for {host} in memory.share.trusted_cas cannot be used: {reason}")]
    Ca {
        host: String,
        path: PathBuf,
        reason: String,
    },
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
    access_token: String,
    refresh_token: String,
    expires_at: DateTime<Utc>,
    scope: String,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 140: share server tokens never reach a log; Debug prints only their expiry"
    )
)]
impl std::fmt::Debug for Tokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Tokens(until {})", self.expires_at)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Logins {
    logins: BTreeMap<String, Tokens>,
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
pub(crate) struct ShareServers {
    policy: SharePolicy,
    default: Result<ShareServer, ShareRefused>,
    store: PathBuf,
    lock: PathBuf,
    cas: Vec<config::model::TrustedCa>,
}

#[derive(Debug, Clone)]
pub(crate) struct Server {
    host: ShareServer,
    base: Url,
    client: reqwest::Client,
    store: PathBuf,
    lock: PathBuf,
}

impl ShareServers {
    pub(crate) async fn from_env(env: &Environment) -> Result<Self, crate::Error> {
        let loaded = crate::db::load(env, None).await?;
        let configured = loaded
            .config
            .share_server()
            .map(|section| section.url.parse())
            .transpose()
            .map_err(ServerError::from)?;
        let policy = SharePolicy::from_config(&loaded.config).map_err(ServerError::from)?;
        let state = capture::share::state_home(env)?.join(capture::sanitize::key::DIR_NAME);
        Ok(Self {
            default: policy.default_server(configured),
            policy,
            store: state.join(STORE_FILE),
            lock: state.join(LOCK_FILE),
            cas: loaded.config.share_trusted_cas().to_vec(),
        })
    }

    pub(crate) fn pick(&self, named: Option<&str>) -> Result<ShareServer, ServerError> {
        match named {
            Some(text) => Ok(text.parse()?),
            None => Ok(self.default.clone()?),
        }
    }

    pub(crate) async fn connect(
        &self,
        host: &ShareServer,
        timeout: Duration,
    ) -> Result<Server, ServerError> {
        self.policy.admit(host)?;
        let base = Url::parse(&host.base_url()).map_err(|_| CodeError::Server(host.to_string()))?;
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .https_only(!host.is_loopback())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(timeout);
        for trusted in &self.cas {
            let names_this = trusted
                .host
                .parse::<ShareServer>()
                .is_ok_and(|named| named == *host);
            if names_this {
                builder = builder.add_root_certificate(read_ca(trusted).await?);
            }
        }
        Ok(Server {
            host: host.clone(),
            base,
            client: builder.build()?,
            store: self.store.clone(),
            lock: self.lock.clone(),
        })
    }

    fn status(&self) -> Result<Report, ServerError> {
        let logins = read_logins(&self.store)?;
        if logins.logins.is_empty() {
            return Ok(Report::success(match &self.default {
                Ok(host) => format!(
                    "not signed in to any share server; rh share login signs in to {host}\n"
                ),
                Err(refused) => format!("not signed in to any share server; {refused}\n"),
            }));
        }
        Ok(Report::success(logins.logins.iter().fold(
            String::new(),
            |mut lines, (server, tokens)| {
                lines.push_str("signed in to ");
                lines.push_str(server);
                lines.push_str(" (scope ");
                lines.push_str(&tokens.scope);
                lines.push_str("); the access token renews itself, next at ");
                lines.push_str(&tokens.expires_at.format("%Y-%m-%d %H:%M UTC").to_string());
                lines.push('\n');
                lines
            },
        )))
    }
}

async fn read_ca(trusted: &config::model::TrustedCa) -> Result<reqwest::Certificate, ServerError> {
    let failed = |reason: String| ServerError::Ca {
        host: trusted.host.clone(),
        path: trusted.ca.clone(),
        reason,
    };
    let metadata = tokio::fs::metadata(&trusted.ca)
        .await
        .map_err(|error| failed(error.to_string()))?;
    if metadata.len() > CA_CAP {
        return Err(failed("it is larger than 1 MiB".to_owned()));
    }
    let pem = tokio::fs::read(&trusted.ca)
        .await
        .map_err(|error| failed(error.to_string()))?;
    reqwest::Certificate::from_pem(&pem).map_err(|error| failed(error.to_string()))
}

fn read_logins(store: &std::path::Path) -> Result<Logins, ServerError> {
    let bytes = match PrivateFile::new(store, STORE_NAME, STORE_CAP).read() {
        Ok(bytes) => bytes,
        Err(KeyError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Logins::default());
        }
        Err(error) => return Err(error.into()),
    };
    serde_json::from_slice(&bytes).map_err(|error| ServerError::Store(error.to_string()))
}

fn write_logins(store: &std::path::Path, logins: &Logins) -> Result<(), ServerError> {
    let parent = store.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut partial = tempfile::Builder::new()
        .prefix(".share-logins-")
        .tempfile_in(parent)?;
    let text = serde_json::to_vec(logins).map_err(|error| ServerError::Store(error.to_string()))?;
    partial.write_all(&text)?;
    partial.as_file().sync_all()?;
    partial
        .persist(store)
        .map_err(|error| ServerError::Io(error.error))?;
    Ok(())
}

impl Server {
    pub(crate) fn endpoint(&self, path: &str) -> Result<Url, ServerError> {
        self.base
            .join(path)
            .map_err(|_| CodeError::Server(self.host.to_string()).into())
    }

    pub(crate) const fn host(&self) -> &ShareServer {
        &self.host
    }

    pub(crate) const fn client(&self) -> &reqwest::Client {
        &self.client
    }

    async fn lock(&self) -> Result<std::fs::File, ServerError> {
        if let Some(parent) = self.lock.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock)?;
        tokio::task::spawn_blocking(move || file.lock().map(|()| file))
            .await
            .map_err(std::io::Error::other)?
            .map_err(ServerError::from)
    }

    fn mine(&self) -> Result<Tokens, ServerError> {
        read_logins(&self.store)?
            .logins
            .remove(self.host.as_str())
            .ok_or_else(|| ServerError::LoggedOut(self.host.clone()))
    }

    fn keep(&self, tokens: Tokens) -> Result<(), ServerError> {
        let mut logins = read_logins(&self.store)?;
        logins.logins.insert(self.host.to_string(), tokens);
        write_logins(&self.store, &logins)
    }

    pub(crate) async fn access_token(&self, rejected: Option<&str>) -> Result<String, ServerError> {
        let tokens = self.mine()?;
        if rejected.is_none() && tokens.expires_at - REFRESH_MARGIN > Utc::now() {
            return Ok(tokens.access_token);
        }
        let _lock = self.lock().await?;
        let tokens = self.mine()?;
        let fresh = tokens.expires_at - REFRESH_MARGIN > Utc::now();
        let renewed_elsewhere = rejected.is_some_and(|token| token != tokens.access_token);
        if (fresh && rejected.is_none()) || renewed_elsewhere {
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
                serde_json::from_str(&body).map_err(|_| ServerError::Status { status, body })?;
            return Err(ServerError::Expired {
                server: self.host.clone(),
                code: error.error,
            });
        }
        let renewed = tokens_from(checked(response).await?.json().await?);
        let access = renewed.access_token.clone();
        self.keep(renewed)?;
        Ok(access)
    }

    async fn start(&self, client_name: &str) -> Result<Device, ServerError> {
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
        Ok(device)
    }

    async fn login(
        &self,
        client_name: &str,
        shutdown: &CancellationToken,
    ) -> Result<Report, ServerError> {
        let device = self.start(client_name).await?;
        let mut interval = Duration::from_secs(device.interval.unwrap_or(5).max(1));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(device.expires_in);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return Err(ServerError::Cancelled),
                () = tokio::time::sleep(interval) => {}
            }
            if tokio::time::Instant::now() > deadline {
                return Err(ServerError::Denied("not given before the code expired"));
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
                let tokens = tokens_from(response.json().await?);
                let scope = tokens.scope.clone();
                let _lock = self.lock().await?;
                self.keep(tokens)?;
                return Ok(Report::success(format!(
                    "signed in to {} (scope {scope}); the login lasts until rh share logout {} or a revoke on the server\n",
                    self.host, self.host
                )));
            }
            let status = response.status().as_u16();
            let body = response.text().await?;
            let error: OauthError =
                serde_json::from_str(&body).map_err(|_| ServerError::Status {
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
                    return Err(ServerError::Denied("denied in the browser"));
                }
                OauthCode::ExpiredToken => {
                    return Err(ServerError::Denied("not given before the code expired"));
                }
                OauthCode::InvalidGrant | OauthCode::Other => {
                    return Err(ServerError::Status { status, body });
                }
            }
        }
    }

    async fn logout(&self) -> Result<Report, ServerError> {
        let _lock = self.lock().await?;
        let mut logins = read_logins(&self.store)?;
        let Some(tokens) = logins.logins.get(self.host.as_str()) else {
            return Ok(Report::success(format!(
                "this machine is not signed in to {}\n",
                self.host
            )));
        };
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
        .await?;
        logins.logins.remove(self.host.as_str());
        write_logins(&self.store, &logins)?;
        Ok(Report::success(format!(
            "signed out of {} and revoked this machine's login there\n",
            self.host
        )))
    }
}

fn tokens_from(issued: Issued) -> Tokens {
    Tokens {
        access_token: issued.access_token,
        refresh_token: issued.refresh_token,
        expires_at: Utc::now() + chrono::TimeDelta::seconds(issued.expires_in),
        scope: issued.scope,
    }
}

pub(crate) async fn checked(response: reqwest::Response) -> Result<reqwest::Response, ServerError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let body = response.text().await?;
    Err(ServerError::Status {
        status,
        body: body.chars().take(200).collect(),
    })
}

pub async fn run(
    command: ShareCommand,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Report, crate::Error> {
    let servers = ShareServers::from_env(env).await?;
    Ok(match command {
        ShareCommand::Login { server } => {
            let host = servers.pick(server.as_deref())?;
            let name = format!("RivetHub CLI on {}", host_name()?);
            servers
                .connect(&host, REQUEST_TIMEOUT)
                .await?
                .login(&name, shutdown)
                .await?
        }
        ShareCommand::Logout { server } => {
            let host = servers.pick(server.as_deref())?;
            servers
                .connect(&host, REQUEST_TIMEOUT)
                .await?
                .logout()
                .await?
        }
        ShareCommand::Status => servers.status()?,
    })
}

fn host_name() -> Result<String, crate::Error> {
    let name = nix::unistd::gethostname()
        .map_err(|_| crate::Error::HostName)?
        .into_string()
        .map_err(|_| crate::Error::HostName)?;
    Ok(name.trim().to_owned())
}
