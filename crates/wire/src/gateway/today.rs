use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::gateway::entities::{ChangeKind, Freshness};
use crate::gateway::review::{ReviewItem, SubjectRef};
use crate::gateway::session_catalog::SessionSummaryRow;
use crate::harness::HarnessId;
use crate::session_id::SessionId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionRef {
    pub kind: SessionKind,
    pub id: SessionId,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SessionKind {
    Session,
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
#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub kind: ChangeKind,
    pub subject: SubjectRef,
    pub text: String,

    #[serde_as(as = "crate::time::Rfc3339")]
    pub at: Timestamp,
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

#[serde_with::serde_as]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActivityDay {
    #[serde_as(as = "crate::time::CivilDate")]
    pub date: Date,
    #[schemars(range(max = 4))]
    pub level: u8,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TodayDigest {
    #[serde_as(as = "crate::time::CivilDate")]
    pub date: Date,
    pub headline: String,
    pub summary: String,
    pub needs_you: Vec<ReviewItem>,
    pub review_count: u32,
    pub entries: Vec<TodayEntry>,
    pub activity_by_day: Vec<ActivityDay>,
    pub recently_updated: Vec<SubjectRef>,
    pub freshness: Freshness,
}
