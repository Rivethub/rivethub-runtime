use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportVerification {
    Verified,
    Unsigned,
    UnknownKey,
    Unreceived,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportedFrom {
    pub author: Option<String>,
    pub origin_date: Option<DateTime<Utc>>,
    pub verification: ImportVerification,
}
