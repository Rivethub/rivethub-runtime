use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use capture::seal::Sealed;
use capture::seal::ShareCode;
use capture::seal::ShareServer;
use capture::share::ShareError as SanitizeShareError;
use capture::share_policy::ShareRefused;
use capture::sign::NodeKey;
use capture::sign::PublicKey;
use chrono::DateTime;
use chrono::Utc;
use config::Environment;
use config::MappingFile;
use kernel::memory::share::desk::DeskError;
use kernel::memory::share::desk::ServerKind;
use kernel::memory::share::desk::ServerView;
use kernel::memory::share::desk::Servers;
use oauth2::DeviceCodeErrorResponseType;
use oauth2::RequestTokenError;
use oauth2::Scope;
use oauth2::StandardDeviceAuthorizationResponse;
use reqwest::StatusCode;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use super::dropbox::DropBox;
use super::dropbox::DropError;
use super::share::ShareError;
use crate::Error;
use crate::share_server::Login;
use crate::share_server::ME_PATH;
use crate::share_server::REQUEST_TIMEOUT;
use crate::share_server::SCOPE;
use crate::share_server::Server;
use crate::share_server::ServerError;
use crate::share_server::ShareServers;
use crate::share_server::TeamLogin;
use crate::share_server::Tokens;
use crate::share_server::checked;
use crate::share_server::logins::Forgot;
use crate::share_server::logins::Listed;
use crate::share_server::oauth;

pub fn node_key(env: &Environment) -> Result<PublicKey, Error> {
    Ok(NodeKey::load_or_create(&signing_key_path(env)?)
        .map_err(ShareError::from)?
        .public_key())
}

pub(super) fn signing_key_path(env: &Environment) -> Result<PathBuf, Error> {
    Ok(NodeKey::path_in(&capture::share::state_home(env)?))
}

pub async fn seal_bytes(
    env: &Environment,
    text: Vec<u8>,
    sign: bool,
    confirmed_flags: usize,
) -> Result<Sealed, Error> {
    let signer = if sign {
        Some(NodeKey::load_or_create(&signing_key_path(env)?).map_err(ShareError::from)?)
    } else {
        None
    };
    Ok(tokio::task::spawn_blocking(move || {
        capture::seal::seal(&text, signer.as_ref(), confirmed_flags)
    })
    .await
    .map_err(std::io::Error::other)?
    .map_err(ShareError::from)?)
}

pub struct Shared {
    pub sessions: usize,
    pub signed: Option<PublicKey>,
    pub uploaded: u64,
    pub expires_at: DateTime<Utc>,
    pub code: String,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 140: the share code's key is a secret; Debug shows only the drop id part"
    )
)]
impl std::fmt::Debug for Shared {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let id = self
            .code
            .split_once('#')
            .map_or(self.code.as_str(), |(id, _)| id);
        formatter
            .debug_struct("Shared")
            .field("sessions", &self.sessions)
            .field("code", &format_args!("{id}#.."))
            .finish_non_exhaustive()
    }
}

pub async fn upload_to(
    env: &Environment,
    server: Option<&str>,
    sealed: Sealed,
) -> Result<Shared, Error> {
    let servers = ShareServers::from_env(env).await?;
    let host = servers.pick(server).map_err(DropError::from)?;
    let drops = DropBox::on(&servers, &host).await?;
    let (id, uploaded) = drops.upload(bytes::Bytes::from(sealed.ciphertext)).await?;
    Ok(Shared {
        sessions: sealed.titles.len(),
        signed: sealed.signed,
        uploaded: uploaded.bytes,
        expires_at: uploaded.expires_at,
        code: ShareCode {
            server: drops.host().clone(),
            id,
            key: sealed.key,
        }
        .reveal(),
    })
}

async fn connect(env: &Environment, server: Option<&str>) -> Result<Server, Error> {
    let servers = ShareServers::from_env(env).await?;
    let host = servers.pick(server)?;
    Ok(servers.connect(&host, REQUEST_TIMEOUT).await?)
}

#[derive(Debug)]
pub struct DeviceLogin {
    pub user_code: String,
    pub verification_uri_complete: String,
    pub expires_in: Duration,
    pub expires_at: DateTime<Utc>,
    pub pending: PendingLogin,
}

pub struct PendingLogin {
    server: ShareServer,
    interval: Arc<AtomicU64>,
    expires_at: DateTime<Utc>,
    cancel: CancellationToken,
    task: Option<AbortOnDropHandle<Result<Approval, Error>>>,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 151: the device code never leaves rh; Debug names only the server"
    )
)]
impl std::fmt::Debug for PendingLogin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "PendingLogin({})", self.server)
    }
}

impl PendingLogin {
    #[must_use]
    pub const fn server(&self) -> &ShareServer {
        &self.server
    }

    #[must_use]
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval.load(Ordering::Relaxed).max(1))
    }

    #[must_use]
    pub const fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    pub async fn poll(&mut self) -> Result<Option<Approval>, Error> {
        match &self.task {
            None => Err(ServerError::Finished.into()),
            Some(task) if !task.is_finished() => Ok(None),
            Some(_) => self.finished().await.map(Some),
        }
    }

    pub async fn cancel(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            drop(task.await);
        }
    }

    pub async fn finish(mut self) -> Result<Approval, Error> {
        self.finished().await
    }

    async fn finished(&mut self) -> Result<Approval, Error> {
        let task = self.task.take().ok_or(ServerError::Finished)?;
        task.await.map_err(std::io::Error::other)?
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    SignedIn { scope: String },
    Denied,
    Expired,
}

pub async fn start(
    env: &Environment,
    server: Option<&str>,
    client: &str,
) -> Result<DeviceLogin, Error> {
    let server = connect(env, server).await?;
    let endpoints = server.endpoints().await?;
    let oauth = Server::oauth_client(&endpoints);
    let details: StandardDeviceAuthorizationResponse = oauth
        .exchange_device_code()
        .add_scopes(SCOPE.split(' ').map(|scope| Scope::new(scope.to_owned())))
        .add_extra_param("client_name", format!("{client} on {}", host_name()?))
        .request_async(&server)
        .await
        .map_err(oauth::failed)?;
    let expires_in = details.expires_in();
    let lifetime = chrono::TimeDelta::from_std(expires_in)
        .map_err(|_| ServerError::Lifetime(expires_in.as_secs()))?;
    let expires_at = Utc::now()
        .checked_add_signed(lifetime)
        .ok_or(ServerError::Lifetime(expires_in.as_secs()))?;
    let interval = Arc::new(AtomicU64::new(details.interval().as_secs()));
    let verification_uri_complete = openable(
        details.verification_uri_complete().map_or_else(
            || details.verification_uri().to_string(),
            |complete| complete.secret().clone(),
        ),
        server.host(),
    )?;
    let user_code = details.user_code().secret().clone();
    let host = server.host().clone();
    let cancel = CancellationToken::new();
    let task = tokio::spawn(approval(
        server,
        oauth,
        details,
        Arc::clone(&interval),
        cancel.clone(),
    ));
    Ok(DeviceLogin {
        verification_uri_complete,
        user_code,
        expires_in,
        expires_at,
        pending: PendingLogin {
            server: host,
            interval,
            expires_at,
            cancel,
            task: Some(AbortOnDropHandle::new(task)),
        },
    })
}

fn openable(uri: String, server: &ShareServer) -> Result<String, ServerError> {
    let url = url::Url::parse(&uri)
        .map_err(|_| ServerError::Protocol("its verification URL is invalid"))?;
    if url.scheme() == "https" || (url.scheme() == "http" && server.is_loopback()) {
        return Ok(uri);
    }
    Err(ServerError::Protocol(
        "its verification URL must be https (http only for a loopback share server)",
    ))
}

async fn approval(
    server: Server,
    oauth: oauth::OauthClient,
    details: StandardDeviceAuthorizationResponse,
    interval: Arc<AtomicU64>,
    cancel: CancellationToken,
) -> Result<Approval, Error> {
    let polling = oauth.exchange_device_access_token(&details).request_async(
        &server,
        |wait| {
            interval.store(wait.as_secs(), Ordering::Relaxed);
            tokio::time::sleep(wait)
        },
        None,
    );
    let asked = tokio::select! {
        () = cancel.cancelled() => return Err(ServerError::Cancelled.into()),
        asked = polling => asked,
    };
    let issued = match asked {
        Ok(issued) => issued,
        Err(RequestTokenError::ServerResponse(refusal)) => match refusal.error() {
            DeviceCodeErrorResponseType::AccessDenied => return Ok(Approval::Denied),
            DeviceCodeErrorResponseType::ExpiredToken => return Ok(Approval::Expired),
            DeviceCodeErrorResponseType::AuthorizationPending
            | DeviceCodeErrorResponseType::SlowDown
            | DeviceCodeErrorResponseType::Basic(_) => {
                return Err(oauth::failed(RequestTokenError::ServerResponse(refusal)).into());
            }
        },
        Err(other) => return Err(oauth::failed(other).into()),
    };
    let tokens = Tokens::issued(&issued, None)?;
    let scope = tokens.scope.clone();
    let _lock = server.lock().await?;
    if cancel.is_cancelled() {
        return Err(ServerError::Cancelled.into());
    }
    server.keep(&Login::Device(tokens)).await?;
    Ok(Approval::SignedIn { scope })
}

#[derive(Deserialize)]
struct Me {
    user: String,
}

pub async fn login_team(env: &Environment, server: &str, token: String) -> Result<String, Error> {
    let server = connect(env, Some(server)).await?;
    let response = server
        .client()
        .get(server.endpoint(ME_PATH)?)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(ServerError::from)?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        return Err(ServerError::TokenRefused {
            server: server.host().clone(),
            status: response.status().as_u16(),
        }
        .into());
    }
    let me: Me = checked(response)?.json().await.map_err(ServerError::from)?;
    let _lock = server.lock().await?;
    server
        .keep(&Login::Team(TeamLogin {
            team_token: token,
            user: me.user.clone(),
        }))
        .await?;
    Ok(me.user)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoggedOut {
    NotSignedIn,
    Revoked,
    Forgotten,
    Refused {
        refused: ShareRefused,
        keyring: Option<String>,
    },
}

pub async fn logout(
    env: &Environment,
    server: Option<&str>,
) -> Result<(ShareServer, LoggedOut), Error> {
    let servers = ShareServers::from_env(env).await?;
    let host = servers.logout_target(server)?;
    let server = match servers.connect(&host, REQUEST_TIMEOUT).await {
        Ok(server) => server,
        Err(ServerError::Policy(refused)) => {
            let outcome = match servers.forget_refused(&host).await? {
                Forgot::Absent => LoggedOut::NotSignedIn,
                Forgot::Clean => LoggedOut::Refused {
                    refused,
                    keyring: None,
                },
                Forgot::SecretKept(reason) => LoggedOut::Refused {
                    refused,
                    keyring: Some(reason),
                },
            };
            return Ok((host, outcome));
        }
        Err(error) => return Err(error.into()),
    };
    let outcome = sign_out(&server).await?;
    Ok((host, outcome))
}

pub(crate) async fn sign_out(server: &Server) -> Result<LoggedOut, ServerError> {
    let host = server.host().clone();
    let _lock = server.lock().await?;
    let login = match server.logins().load(&host).await {
        Err(ServerError::LoggedOut(_)) => return Ok(LoggedOut::NotSignedIn),
        loaded => loaded?,
    };
    let outcome = match login {
        Login::Team(_) => LoggedOut::Forgotten,
        Login::Device(tokens) => {
            let endpoints = server.endpoints().await?;
            server.revoke(&endpoints, &tokens.refresh_token).await?;
            LoggedOut::Revoked
        }
    };
    server.logins().forget(&host).await?;
    Ok(outcome)
}

pub async fn servers(env: &Environment) -> Result<Servers, Error> {
    let servers = ShareServers::from_env(env).await?;
    let default = match servers.default_host() {
        Ok(host) => Some(host.to_string()),
        Err(_refused) => None,
    };
    let store = servers.logins();
    let logins = store.list()?;
    let mut views: Vec<ServerView> = Vec::new();
    if let Some(default) = &default
        && !logins.contains_key(default)
    {
        views.push(ServerView {
            origin: default.clone().into(),
            kind: ServerKind::Cloud,
            user: None,
            signed_in: false,
            default: true,
        });
    }
    for (host, login) in logins {
        let (kind, user) = match login {
            Listed::Device { .. } => (ServerKind::Cloud, None),
            Listed::Team { user } => (ServerKind::Team, Some(user)),
        };
        views.push(ServerView {
            default: default.as_deref() == Some(host.as_str()),
            origin: host.into(),
            kind,
            user,
            signed_in: true,
        });
    }
    views.sort_by_key(|view| !view.default);
    Ok(Servers {
        servers: views,
        logins_file: store.file().display().to_string(),
        token_store: store.kind(),
    })
}

pub(crate) fn host_name() -> Result<String, Error> {
    let name = nix::unistd::gethostname()
        .map_err(|_| Error::HostName)?
        .into_string()
        .map_err(|_| Error::HostName)?;
    Ok(name.trim().to_owned())
}

#[derive(Debug)]
pub struct Offered {
    pub message: String,
    pub write: Option<(PathBuf, String)>,
}

pub async fn offer(
    mapping: &MappingFile,
    maps: &[(&str, &str)],
    learn: bool,
) -> Result<Offered, Error> {
    let path = match mapping {
        MappingFile::Configured(path) | MappingFile::Default(path) => path,
    };
    let existing = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(SanitizeShareError::MappingRead {
                path: path.display().to_string(),
                source,
            }
            .into());
        }
    };
    let invalid = |source| -> Error {
        SanitizeShareError::Mapping {
            path: path.display().to_string(),
            source,
        }
        .into()
    };
    let known = capture::sanitize::mapping::parse(&existing).map_err(invalid)?;
    let fresh: Vec<String> = maps
        .iter()
        .filter(|(term, _)| !known.learned(term))
        .map(|(term, placeholder)| capture::sanitize::mapping::map_rule(term, placeholder))
        .collect();
    let display = path.display();
    let told = |message: String| Offered {
        message,
        write: None,
    };
    if fresh.is_empty() {
        return Ok(told(format!(
            "{display} already has a rule for each map mark\n"
        )));
    }
    let count = fresh.len();
    let noun = if count == 1 { "rule" } else { "rules" };
    let rules = fresh.join("\n");
    if !learn {
        return Ok(told(format!(
            "rerun with --learn to add {count} map {noun} to {display}:\n\n{rules}"
        )));
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&rules);
    capture::sanitize::mapping::parse(&text).map_err(invalid)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    Ok(Offered {
        message: format!("added {count} map {noun} to {display}\n"),
        write: Some((path.clone(), text)),
    })
}

#[must_use]
pub fn desk_error(error: Error) -> DeskError {
    if let Error::ShareServer(server) | Error::Drop(DropError::Server(server)) = error {
        server_error(server)
    } else {
        DeskError::Failed(error.to_string())
    }
}

fn server_error(error: ServerError) -> DeskError {
    match error {
        ServerError::LoggedOut(server) => DeskError::SignedOut(server.to_string().into()),
        ServerError::Expired { server, .. } => DeskError::LoginExpired(server.to_string().into()),
        ServerError::Denied(reason) => DeskError::LoginDenied(reason),
        ServerError::TokenRefused { status, .. } => DeskError::BadToken(format!("HTTP {status}")),
        ServerError::Http(error) => DeskError::Unreachable(error.to_string()),
        error @ ServerError::Unavailable { .. } => DeskError::Unreachable(error.to_string()),
        error @ ServerError::Answered(status) => DeskError::DropBox {
            status,
            message: error.to_string(),
        },
        ServerError::Finished => DeskError::UnknownLogin,
        ServerError::Server(error) => DeskError::BadServer(error.to_string()),
        error @ ServerError::Policy(_) => DeskError::BadServer(error.to_string()),
        other @ (ServerError::Store(_)
        | ServerError::Keyring(_)
        | ServerError::Ca { .. }
        | ServerError::File(_)
        | ServerError::Io(_)
        | ServerError::Lifetime(_)
        | ServerError::AllowedServer(_)
        | ServerError::Refused(_)
        | ServerError::Protocol(_)
        | ServerError::NoDeviceFlow(_)
        | ServerError::Cancelled) => DeskError::Failed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_http_opens_only_for_a_loopback_share_server() {
        let page = "http://share.example.com/device";
        let loopback: ShareServer = "127.0.0.1:8443".parse().expect("loopback");
        let remote: ShareServer = "share.example.com".parse().expect("remote");

        assert_eq!(
            openable(page.to_owned(), &loopback).ok().as_deref(),
            Some(page)
        );
        assert!(openable(page.to_owned(), &remote).is_err());
        assert!(openable("https://share.example.com/device".to_owned(), &remote).is_ok());
        assert!(openable("data:text/html,hi".to_owned(), &loopback).is_err());
    }
}
