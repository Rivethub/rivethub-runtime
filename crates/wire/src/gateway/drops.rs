use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Uploaded {
    pub id: String,

    #[serde_as(as = "crate::time::Rfc3339")]
    pub expires_at: Timestamp,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DropRefusal {
    pub error: String,
    pub status: u16,
}
