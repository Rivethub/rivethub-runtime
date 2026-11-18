use jiff::Timestamp;
use serde::{Deserialize, Serialize};

pub const REAUTH_REQUIRED: &str = "reauth_required";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
#[schemars(
    description = "A one-time code, session token or CSRF token. Debug never prints it, so logging a body cannot leak it."
)]
pub struct Secret(String);

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret(..)")
    }
}

impl Secret {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoginCodeResponse {
    pub code: Secret,
    pub expires_in_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRequest {
    pub code: Secret,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionResponse {
    pub csrf: Secret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RotateResponse {
    pub rotated: bool,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, strum::Display,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum RefusalKind {
    Unauthorized,
    InvalidToken,
    InsufficientScope,
}

#[serde_with::skip_serializing_none]
#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefusedRequest {
    #[serde_as(as = "crate::time::Rfc3339")]
    pub at: Timestamp,
    pub kind: RefusalKind,
    pub method: String,
    pub route: String,
    pub peer: String,
    #[schemars(description = "The first eight hex digits of the presented token's SHA-256.")]
    pub token: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthRefusals {
    pub unauthorized: u64,
    pub invalid_token: u64,
    pub insufficient_scope: u64,
    #[schemars(description = "The latest request refused with a token presented.")]
    pub last: Option<RefusedRequest>,
}
