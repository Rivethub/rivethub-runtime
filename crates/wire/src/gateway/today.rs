use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::gateway::entities::{ChangeKind, Freshness};
use crate::gateway::review::{ReviewItem, SubjectRef};
use crate::gateway::session_catalog::SessionSummaryRow;
use crate::harness::HarnessId;
use crate::session_id::SessionId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename = "session")]
#[schemars(rename = "SessionRef", transform = session_kind)]
pub struct SessionRef {
    pub id: SessionId,
    pub title: String,
}

// schemars drops the tag serde writes on a struct.
fn session_kind(schema: &mut schemars::Schema) {
    if let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    {
        properties.insert(
            "kind".to_owned(),
            serde_json::json!({"type": "string", "const": "session"}),
        );
    }
    if let Some(required) = schema
        .get_mut("required")
        .and_then(serde_json::Value::as_array_mut)
    {
        required.insert(0, "kind".into());
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TodayDigest {
    pub date: NaiveDate,
    pub headline: String,
    pub summary: String,
    pub needs_you: Vec<ReviewItem>,
    pub review_count: u32,
    pub entries: Vec<TodayEntry>,
    pub activity_by_day: Vec<ActivityDay>,
    pub recently_updated: Vec<SubjectRef>,
    pub freshness: Freshness,
}
