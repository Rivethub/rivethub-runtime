use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use capture::sanitize::KeyError;
use capture::seal::CodeError;
use capture::seal::ShareServer;
use capture::share_policy::{BadAllowedServer, SharePolicy, ShareRefused};
use chrono::DateTime;
use chrono::Utc;
use clap::Subcommand;
use config::Environment;
use kernel::memory::share::desk::TokenStore;
use oauth2::RefreshToken;
use oauth2::RequestTokenError;
use oauth2::TokenResponse as _;
use oauth2::basic::BasicTokenResponse;
use serde::Deserialize;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::Report;
use crate::memory::desk;
use crate::memory::desk::Approval;
use crate::memory::desk::LoggedOut;
use logins::Forgot;
use logins::Listed;
use logins::LoginStore;

pub(crate) mod logins;
pub(crate) mod oauth;

pub(crate) const CLIENT_ID: &str = "rivethub-cli";
pub(crate) const SCOPE: &str = "drops:write offline_access";
pub(crate) const STORE_FILE: &str = "share-logins.json";
pub(crate) const ME_PATH: &str = "api/me";
const LOCK_FILE: &str = "share-logins.json.lock";
const CA_CAP: u64 = 1024 * 1024;
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const LIFETIME_WHEN_UNSTATED: Duration = Duration::from_mins(15);
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
    Policy(#[from] ShareRefused),
    #[error("this machine is not signed in to {0}; run rh share login {0}")]
    LoggedOut(ShareServer),
    #[error("the login on {server} expired or was revoked; run rh share login {server} ({code})")]
    Expired {
        server: ShareServer,
        code: OauthErrorCode,
    },
    #[error("the browser approval was {0}; run rh share login to try again")]
    Denied(&'static str),
    #[error("{server} refused the team token (HTTP {status}); sign in again with a valid token")]
    TokenRefused { server: ShareServer, status: u16 },
    #[error("the share server could not be reached: {0}")]
    Http(#[from] reqwest::Error),
    #[error("the share server refused the request ({0})")]
    Refused(OauthErrorCode),
    #[error("the share server answered {0}")]
    Answered(u16),
    #[error("the share server is busy or down (HTTP {status}); try again {}", retry_hint(*.retry_after))]
    Unavailable {
        status: u16,
        retry_after: Option<Duration>,
    },
    #[error("the share server does not speak OAuth as expected: {0}")]
    Protocol(&'static str),
    #[error(
        "{0} offers no device sign-in (no OAuth metadata at /.well-known/oauth-authorization-server)"
    )]
    NoDeviceFlow(ShareServer),
    #[error("the share server gave a device code lifetime of {0} seconds, which is out of range")]
    Lifetime(u64),
    #[error("the share login file is unreadable: {0}")]
    Store(String),
    #[error(
        "the OS keyring could not be used: {0}; set RIVETHUB_TOKEN_STORE=file to keep share tokens in the 0600 login file"
    )]
    Keyring(String),
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
    #[error("the sign-in already finished")]
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum OauthErrorCode {
    InvalidRequest,
    InvalidClient,
    InvalidGrant,
    UnauthorizedClient,
    UnsupportedGrantType,
    InvalidScope,
    AuthorizationPending,
    SlowDown,
    AccessDenied,
    ExpiredToken,
    UnsupportedTokenType,
    #[serde(other)]
    #[strum(serialize = "unrecognised error")]
    Other,
}

fn retry_hint(wait: Option<Duration>) -> String {
    wait.map_or_else(
        || "shortly".to_owned(),
        |wait| format!("in {} seconds", wait.as_secs()),
    )
}

#[derive(Clone)]
pub(crate) struct Tokens {
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
    pub(crate) expires_at: DateTime<Utc>,
    pub(crate) scope: String,
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

#[derive(Clone)]
pub(crate) struct TeamLogin {
    pub(crate) team_token: String,
    pub(crate) user: String,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 140: a team token never reaches a log; Debug prints only its user"
    )
)]
impl std::fmt::Debug for TeamLogin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "TeamLogin({})", self.user)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Login {
    Device(Tokens),
    Team(TeamLogin),
}

#[derive(Debug, Clone)]
pub(crate) struct ShareServers {
    policy: SharePolicy,
    configured: Option<ShareServer>,
    logins: LoginStore,
    lock: PathBuf,
    cas: Vec<config::model::TrustedCa>,
}

#[derive(Debug, Clone)]
pub(crate) struct Server {
    host: ShareServer,
    base: Url,
    client: reqwest::Client,
    logins: LoginStore,
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
            configured,
            policy,
            logins: LoginStore::choose(env, state.join(STORE_FILE)).await?,
            lock: state.join(LOCK_FILE),
            cas: loaded.config.share_trusted_cas().to_vec(),
        })
    }

    pub(crate) fn pick(&self, named: Option<&str>) -> Result<ShareServer, ServerError> {
        match named {
            Some(text) => Ok(text.parse()?),
            None => Ok(self.policy.default_server(self.configured.clone())?),
        }
    }

    pub(crate) fn logout_target(&self, named: Option<&str>) -> Result<ShareServer, ServerError> {
        match named {
            Some(text) => Ok(text.parse()?),
            None => Ok(self
                .configured
                .clone()
                .unwrap_or_else(ShareServer::rivethub_cloud)),
        }
    }

    pub(crate) async fn forget_refused(&self, host: &ShareServer) -> Result<Forgot, ServerError> {
        let _lock = lock(&self.lock).await?;
        self.logins.forget_record(host).await
    }

    pub(crate) async fn connect(
        &self,
        host: &ShareServer,
        timeout: Duration,
    ) -> Result<Server, ServerError> {
        self.policy.admit(host)?;
        let base = Url::parse(&host.base_url()).map_err(|_| CodeError::Server)?;
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
            logins: self.logins.clone(),
            lock: self.lock.clone(),
        })
    }

    pub(crate) fn default_host(&self) -> Result<ShareServer, ShareRefused> {
        self.policy.default_server(self.configured.clone())
    }

    pub(crate) const fn logins(&self) -> &LoginStore {
        &self.logins
    }

    fn status(&self) -> Result<Report, ServerError> {
        let listed = self.logins.list()?;
        let kept = match self.logins.kind() {
            TokenStore::Keyring => "tokens are kept in the OS keyring\n".to_owned(),
            TokenStore::File => format!(
                "tokens are kept in {} (mode 0600)\n",
                self.logins.file().display()
            ),
        };
        if listed.is_empty() {
            return Ok(Report::success(
                match self.policy.default_server(self.configured.clone()) {
                    Ok(host) => format!(
                        "not signed in to any share server; rh share login signs in to {host}\n{kept}"
                    ),
                    Err(refused) => format!("not signed in to any share server; {refused}\n{kept}"),
                },
            ));
        }
        let mut lines = listed
            .iter()
            .fold(String::new(), |mut lines, (server, login)| {
                lines.push_str("signed in to ");
                lines.push_str(server);
                match login {
                    Listed::Device { scope, expires_at } => {
                        lines.push_str(" (scope ");
                        lines.push_str(scope);
                        lines.push_str("); the access token renews itself, next at ");
                        lines.push_str(&expires_at.format("%Y-%m-%d %H:%M UTC").to_string());
                    }
                    Listed::Team { user } => {
                        lines.push_str(" as ");
                        lines.push_str(user);
                        lines.push_str(" with a team token");
                    }
                }
                lines.push('\n');
                lines
            });
        lines.push_str(&kept);
        Ok(Report::success(lines))
    }
}

async fn lock(path: &std::path::Path) -> Result<std::fs::File, ServerError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    tokio::task::spawn_blocking(move || file.lock().map(|()| file))
        .await
        .map_err(std::io::Error::other)?
        .map_err(ServerError::from)
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

impl Server {
    pub(crate) fn endpoint(&self, path: &str) -> Result<Url, ServerError> {
        self.base.join(path).map_err(|_| CodeError::Server.into())
    }

    pub(crate) const fn host(&self) -> &ShareServer {
        &self.host
    }

    pub(crate) const fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub(crate) async fn lock(&self) -> Result<std::fs::File, ServerError> {
        lock(&self.lock).await
    }

    pub(crate) const fn logins(&self) -> &LoginStore {
        &self.logins
    }

    async fn device(&self, rejected: Option<&str>) -> Result<Result<Tokens, String>, ServerError> {
        match self.logins.load(&self.host).await? {
            Login::Device(tokens) => Ok(Ok(tokens)),
            Login::Team(team) if rejected.is_none() => Ok(Err(team.team_token)),
            Login::Team(_) => Err(ServerError::TokenRefused {
                server: self.host.clone(),
                status: 401,
            }),
        }
    }

    pub(crate) async fn keep(&self, login: &Login) -> Result<(), ServerError> {
        self.logins.save(&self.host, login).await
    }

    pub(crate) async fn access_token(&self, rejected: Option<&str>) -> Result<String, ServerError> {
        let tokens = match self.device(rejected).await? {
            Ok(tokens) => tokens,
            Err(team) => return Ok(team),
        };
        if rejected.is_none() && tokens.expires_at - REFRESH_MARGIN > Utc::now() {
            return Ok(tokens.access_token);
        }
        let _lock = self.lock().await?;
        let tokens = match self.device(rejected).await? {
            Ok(tokens) => tokens,
            Err(team) => return Ok(team),
        };
        let fresh = tokens.expires_at - REFRESH_MARGIN > Utc::now();
        let renewed_elsewhere = rejected.is_some_and(|token| token != tokens.access_token);
        if (fresh && rejected.is_none()) || renewed_elsewhere {
            return Ok(tokens.access_token);
        }
        let endpoints = self.endpoints().await?;
        let refresh = RefreshToken::new(tokens.refresh_token.clone());
        let refreshed = Self::oauth_client(&endpoints)
            .exchange_refresh_token(&refresh)
            .request_async(self)
            .await;
        let renewed = match refreshed {
            Ok(issued) => Tokens::issued(&issued, Some(&tokens))?,
            Err(RequestTokenError::ServerResponse(refusal)) => {
                return Err(ServerError::Expired {
                    server: self.host.clone(),
                    code: oauth::code(refusal.error().as_ref())?,
                });
            }
            Err(other) => return Err(oauth::failed(other)),
        };
        let access = renewed.access_token.clone();
        self.keep(&Login::Device(renewed)).await?;
        Ok(access)
    }
}

impl Tokens {
    pub(crate) fn issued(
        issued: &BasicTokenResponse,
        previous: Option<&Self>,
    ) -> Result<Self, ServerError> {
        let refresh_token = issued
            .refresh_token()
            .map(|token| token.secret().clone())
            .or_else(|| previous.map(|tokens| tokens.refresh_token.clone()))
            .ok_or_else(|| ServerError::Protocol("the token response has no refresh_token"))?;
        let scope = issued
            .scopes()
            .map(|scopes| {
                scopes
                    .iter()
                    .map(|scope| scope.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .or_else(|| previous.map(|tokens| tokens.scope.clone()))
            .unwrap_or_else(|| SCOPE.to_owned());
        let lifetime = issued.expires_in().unwrap_or(LIFETIME_WHEN_UNSTATED);
        let out_of_range = || ServerError::Lifetime(lifetime.as_secs());
        let expires_at = Utc::now()
            .checked_add_signed(chrono::TimeDelta::from_std(lifetime).map_err(|_| out_of_range())?)
            .ok_or_else(out_of_range)?;
        Ok(Self {
            access_token: issued.access_token().secret().clone(),
            refresh_token,
            expires_at,
            scope,
        })
    }
}

pub(crate) fn checked(response: reqwest::Response) -> Result<reqwest::Response, ServerError> {
    if response.status().is_success() {
        return Ok(response);
    }
    Err(ServerError::Answered(response.status().as_u16()))
}

pub async fn run(
    command: ShareCommand,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Report, crate::Error> {
    let servers = ShareServers::from_env(env).await?;
    Ok(match command {
        ShareCommand::Login { server } => login(env, server.as_deref(), shutdown).await?,
        ShareCommand::Logout { server } => {
            let (host, outcome) = desk::logout(env, server.as_deref()).await?;
            Report::success(match outcome {
                LoggedOut::NotSignedIn => format!("this machine is not signed in to {host}\n"),
                LoggedOut::Revoked => {
                    format!("signed out of {host} and revoked this machine's login there\n")
                }
                LoggedOut::Forgotten => {
                    format!("signed out of {host} and forgot its team token on this machine\n")
                }
                LoggedOut::Refused { refused, keyring } => {
                    let kept = keyring.map_or_else(String::new, |reason| {
                        format!("; its secret stays in the OS keyring because {reason}")
                    });
                    format!(
                        "signed out of {host}; the login was not revoked there because {refused}{kept}\n"
                    )
                }
            })
        }
        ShareCommand::Status => servers.status()?,
    })
}

async fn login(
    env: &Environment,
    server: Option<&str>,
    shutdown: &CancellationToken,
) -> Result<Report, crate::Error> {
    let started = desk::start(env, server, "RivetHub CLI").await?;
    writeln!(
        std::io::stderr(),
        "Open {} and approve the code {} (it expires in {} minutes).",
        started.verification_uri_complete,
        started.user_code,
        started.expires_in.as_secs().div_ceil(60)
    )?;
    let host = started.pending.server().clone();
    let approval = tokio::select! {
        () = shutdown.cancelled() => return Err(ServerError::Cancelled.into()),
        approval = started.pending.finish() => approval?,
    };
    match approval {
        Approval::SignedIn { scope } => Ok(Report::success(format!(
            "signed in to {host} (scope {scope}); the login lasts until rh share logout {host} or a revoke on the server\n"
        ))),
        Approval::Denied => Err(ServerError::Denied("denied in the browser").into()),
        Approval::Expired => Err(ServerError::Denied("not given before the code expired").into()),
    }
}
