pub use kernel::account::{AccountLogin, LoginProgress};

impl From<kernel::account::LoginError> for super::AccountError {
    fn from(error: kernel::account::LoginError) -> Self {
        match error {
            kernel::account::LoginError::CloudReply => Self::CloudReply,
            kernel::account::LoginError::NoSignIn => Self::NoSignIn,
            kernel::account::LoginError::SignedIn => Self::SignedIn,
        }
    }
}
