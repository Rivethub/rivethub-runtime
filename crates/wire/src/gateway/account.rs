use serde::{Deserialize, Serialize};
use url::Url;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum UpdateSource {
    RivethubCloud,
    Github,
}

#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum::Display,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum ReleaseChannel {
    #[default]
    Stable,
    Beta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PendingUpdate {
    pub version: String,
    pub notes_url: Url,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a JSON field of GET /api/account (CONTRACT notes 131 and 145)"
)]
pub struct Account {
    #[serde(default)]
    pub email: Option<String>,
    pub update_source: UpdateSource,
    pub channel: ReleaseChannel,
    pub auto_download: bool,
    pub version: String,
    #[serde(default)]
    pub pending_update: Option<PendingUpdate>,
    pub crash_reports: bool,
    pub usage_counts: bool,
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub cloud_disabled: bool,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountUpdate {
    #[serde(default)]
    pub update_source: Option<UpdateSource>,
    #[serde(default)]
    pub channel: Option<ReleaseChannel>,
    #[serde(default)]
    pub auto_download: Option<bool>,
    #[serde(default)]
    pub crash_reports: Option<bool>,
    #[serde(default)]
    pub usage_counts: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        opaque,
        reason = "CONTRACT note 155: device sign-in has no input fields; deny_unknown_fields rejects rather than discards contents"
    )
)]
pub struct SignInRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SignInStarted {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub poll_interval_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum SignInProgress {
    Pending,
    Expired,
    Denied,
    SignedIn { account: Account },
}

#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum::Display,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum HubKind {
    Local,
    #[default]
    Team,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Hub {
    pub id: String,
    pub name: String,
    pub kind: HubKind,
    pub url: String,
    pub online: bool,
    pub detail: String,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountDeleted {
    #[serde(flatten)]
    pub account: Account,
    pub account_url: Url,
}
