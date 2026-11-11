use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::harness::HarnessId;
use crate::session_id::{SessionId, SessionKey};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleted {
    pub ok: bool,
    pub session: SessionKey,
    pub conversations: u64,
    pub messages: u64,
    pub summaries: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleteRequest {
    pub sessions: Vec<SessionKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum SessionDeleteError {
    #[serde(rename = "not found")]
    NotFound,
    #[serde(rename = "delete failed")]
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleteRefused {
    pub session: SessionKey,
    pub ok: bool,
    pub error: SessionDeleteError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum SessionDeleteResult {
    Deleted(SessionDeleted),
    Refused(SessionDeleteRefused),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleteResults {
    pub results: Vec<SessionDeleteResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SessionTrashReason {
    Empty,
    OneWordReply,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionTrashCandidate {
    pub session: SessionId,
    pub harness: HarnessId,
    pub title: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub reason: SessionTrashReason,
    pub user_turns: u64,
    pub reply: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionTrashList {
    pub candidates: Vec<SessionTrashCandidate>,
}
