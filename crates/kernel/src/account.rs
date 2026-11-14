use async_trait::async_trait;

#[derive(Debug)]
pub struct DeviceLogin {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub poll_interval_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("cloud login is unavailable")]
    CloudReply,
    #[error("no sign-in is waiting")]
    NoSignIn,
    #[error("already signed in")]
    SignedIn,
}

#[async_trait]
pub trait AccountLogin: Send + Sync + std::fmt::Debug {
    async fn start(&self) -> Result<DeviceLogin, LoginError>;
    async fn poll(&self) -> Result<LoginProgress, LoginError>;
    async fn identity(&self) -> Result<Option<(String, String)>, LoginError>;
    async fn sign_out(&self, revoke: bool) -> Result<(), LoginError>;
}

#[derive(Debug)]
pub enum LoginProgress {
    Pending,
    SignedIn,
    Expired,
    Denied,
}
