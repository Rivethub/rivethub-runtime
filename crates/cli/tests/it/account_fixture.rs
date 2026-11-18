use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LoginFile {
    pub(crate) logins: BTreeMap<String, TokenRecord>,
}

#[serde_with::serde_as]
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TokenRecord {
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,

    #[serde_as(as = "wire::time::Rfc3339")]
    pub(crate) expires_at: Timestamp,
    pub(crate) scope: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct CloudAccount {
    pub(crate) email: &'static str,
}
