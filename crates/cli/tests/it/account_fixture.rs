use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LoginFile {
    pub(crate) logins: BTreeMap<String, TokenRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TokenRecord {
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
    pub(crate) expires_at: chrono::DateTime<chrono::Utc>,
    pub(crate) scope: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct CloudAccount {
    pub(crate) email: &'static str,
}
