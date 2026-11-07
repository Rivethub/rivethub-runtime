use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::gateway::entities::{ChangeKind, Freshness};
use crate::gateway::session_catalog::SessionSummaryRow;
use crate::harness::HarnessId;
use crate::session_id::SessionId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SubjectRef {
    Topic { id: String, title: String },
    Entity { id: String, title: String },
    Session { id: SessionId, title: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename = "session")]
pub struct SessionRef {
    pub id: SessionId,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Actor {
    You,
    WikiWriter,
    Categorizer,
    Summarizer,
    Capture { harness: HarnessId },
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub kind: ChangeKind,
    pub subject: SubjectRef,
    pub text: String,
    pub at: DateTime<Utc>,
    pub actor: Actor,
    #[serde(default)]
    pub source: Option<SessionRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TodayEntry {
    pub session: SessionSummaryRow,
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActivityDay {
    pub date: NaiveDate,
    #[schemars(range(max = 4))]
    pub level: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum PendingReview {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TodayDigest {
    pub date: NaiveDate,
    pub headline: String,
    pub summary: String,
    pub needs_you: Vec<PendingReview>,
    pub review_count: u32,
    pub entries: Vec<TodayEntry>,
    pub activity_by_day: Vec<ActivityDay>,
    pub recently_updated: Vec<SubjectRef>,
    pub freshness: Freshness,
}
