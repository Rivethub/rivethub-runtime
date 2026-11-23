use serde::Deserialize;
use serde::Serialize;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum DumpSource {
    Local,
    Cloud,
    Datahub,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpOrigin {
    pub source: DumpSource,
    pub host: String,
}
