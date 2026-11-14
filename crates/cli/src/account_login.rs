use crate::memory::desk::{self, Approval, PendingLogin};
use crate::share_server::{REQUEST_TIMEOUT, ServerError, ShareServers};
use config::Environment;
use kernel::account::DeviceLogin as SignInStarted;
use kernel::account::{AccountLogin, LoginError as AccountError, LoginProgress};

#[derive(serde::Deserialize)]
struct Me {
    email: String,
}

#[derive(Debug)]
pub struct NodeAccountLogin {
    env: Environment,
    host: String,
    pending: tokio::sync::Mutex<Option<PendingLogin>>,
}

impl NodeAccountLogin {
    pub fn new(env: Environment, base: &url::Url) -> Self {
        Self {
            env,
            host: base[url::Position::BeforeHost..url::Position::AfterPort].to_owned(),
            pending: tokio::sync::Mutex::new(None),
        }
    }

    async fn server(&self) -> Result<crate::share_server::Server, AccountError> {
        let servers = ShareServers::from_env(&self.env)
            .await
            .map_err(|_| AccountError::CloudReply)?;
        let host = servers
            .pick(Some(&self.host))
            .map_err(|_| AccountError::CloudReply)?;
        servers
            .connect(&host, REQUEST_TIMEOUT)
            .await
            .map_err(|_| AccountError::CloudReply)
    }
}

#[async_trait::async_trait]
impl AccountLogin for NodeAccountLogin {
    async fn start(&self) -> Result<SignInStarted, AccountError> {
        if self.identity().await?.is_some() {
            return Err(AccountError::SignedIn);
        }
        let mut pending = self.pending.lock().await;
        if let Some(old) = pending.as_mut() {
            old.cancel().await;
        }
        *pending = None;
        let started = desk::start(&self.env, Some(&self.host), "RivetHub app")
            .await
            .map_err(|_| AccountError::CloudReply)?;
        let reply = SignInStarted {
            user_code: started.user_code,
            verification_uri: started.verification_uri_complete,
            expires_in: started.expires_in.as_secs(),
            poll_interval_ms: started
                .pending
                .interval()
                .as_millis()
                .try_into()
                .map_err(|_| AccountError::CloudReply)?,
        };
        *pending = Some(started.pending);
        drop(pending);
        Ok(reply)
    }

    async fn poll(&self) -> Result<LoginProgress, AccountError> {
        let mut pending = self.pending.lock().await;
        let result = pending.as_mut().ok_or(AccountError::NoSignIn)?.poll().await;
        match result {
            Ok(None) => Ok(LoginProgress::Pending),
            Ok(Some(approval)) => {
                *pending = None;
                drop(pending);
                Ok(match approval {
                    Approval::SignedIn { .. } => LoginProgress::SignedIn,
                    Approval::Denied => LoginProgress::Denied,
                    Approval::Expired => LoginProgress::Expired,
                })
            }
            Err(_) => {
                *pending = None;
                drop(pending);
                Err(AccountError::CloudReply)
            }
        }
    }

    async fn identity(&self) -> Result<Option<(String, String)>, AccountError> {
        let server = self.server().await?;
        let token = match server.access_token(None).await {
            Ok(token) => token,
            Err(ServerError::LoggedOut(_)) => return Ok(None),
            Err(ServerError::Expired { .. }) => {
                let _lock = server.lock().await.map_err(|_| AccountError::CloudReply)?;
                server
                    .logins()
                    .forget(server.host())
                    .await
                    .map_err(|_| AccountError::CloudReply)?;
                return Ok(None);
            }
            Err(_) => return Err(AccountError::CloudReply),
        };
        let response = server
            .client()
            .get(
                server
                    .endpoint("v1/account")
                    .map_err(|_| AccountError::CloudReply)?,
            )
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|_| AccountError::CloudReply)?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            let _lock = server.lock().await.map_err(|_| AccountError::CloudReply)?;
            server
                .logins()
                .forget(server.host())
                .await
                .map_err(|_| AccountError::CloudReply)?;
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(AccountError::CloudReply);
        }
        let me: Me = response
            .json()
            .await
            .map_err(|_| AccountError::CloudReply)?;
        Ok(Some((token, me.email)))
    }

    async fn sign_out(&self, revoke: bool) -> Result<(), AccountError> {
        let mut pending = self.pending.lock().await;
        if let Some(old) = pending.as_mut() {
            old.cancel().await;
        }
        *pending = None;
        drop(pending);
        let servers = ShareServers::from_env(&self.env)
            .await
            .map_err(|_| AccountError::CloudReply)?;
        let host = self.host.parse().map_err(|_| AccountError::CloudReply)?;
        if revoke && let Ok(server) = servers.connect(&host, REQUEST_TIMEOUT).await {
            drop(desk::sign_out(&server).await);
        }
        servers
            .forget_refused(&host)
            .await
            .map_err(|_| AccountError::CloudReply)?;
        Ok(())
    }
}
