use chrono::DateTime;
use chrono::NaiveDate;
use chrono::Utc;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use super::super::builder::WikiWriteError;
use crate::BoxError;
use crate::queue::JobId;

pub const ENTITY_PIPELINE_VERSION: i32 = 3;
pub const EXTRACTION_TABLE: &str = "rh_entity_extractions";
pub const KNOWN_LIMIT: i64 = 60;
pub const HISTORY_CHARS: usize = 180;
pub(super) const RENAMED: &str = "renamed";
pub(super) const CONTRADICTED: &str = "contradicted";

pub const ENTITY_LOCK_CLASS: i32 = 0x656e_7479;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtractEntities {
    pub summary_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<Uuid>,

    #[serde(skip)]
    pub session_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityLeaf {
    pub id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub content: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub owner: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TranscriptMessage {
    pub id: Uuid,
    pub at: DateTime<Utc>,
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            stringly,
            reason = "a transcript speaker is harness text, not the worker Role enum"
        )
    )]
    pub role: String,
    pub tool_name: Option<String>,
    pub content: String,
    pub tool_args: Option<Value>,
    pub tool_result: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeafClock {
    pub id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl From<&EntityLeaf> for LeafClock {
    fn from(leaf: &EntityLeaf) -> Self {
        Self {
            id: leaf.id,
            conversation_id: leaf.conversation_id,
            start: leaf.start,
            end: leaf.end,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityWork {
    Pending(EntityLeaf),
    Complete { entities: Vec<String> },
}

fn lenient_uuid<'de, D>(deserializer: D) -> Result<Option<Uuid>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let text: Option<String> = Option::deserialize(deserializer)?;
    let Some(value) = text else {
        return Ok(None);
    };
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a model uuid that does not parse is no id"
        )
    )]
    let parsed = Uuid::parse_str(value.trim()).ok();
    Ok(parsed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowingLeaf {
    pub leaf: EntityLeaf,
    pub done: bool,
    pub job: Option<JobId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct EntityObservations {
    #[serde(default)]
    pub observations: Vec<EntityObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct EntityObservation {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            customserde,
            reason = "the model sends a uuid, a string, or nothing for one field"
        )
    )]
    #[serde(default, alias = "entityRef", deserialize_with = "lenient_uuid")]
    #[schemars(with = "Option<String>")]
    pub entity_ref: Option<Uuid>,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type", alias = "typeName")]
    pub type_name: String,
    #[serde(default)]
    pub aliases: Vec<String>,

    #[serde(default)]
    pub mention: String,
    #[serde(default)]
    pub facts: Vec<ObservedFact>,
    #[serde(default)]
    pub history: Vec<ObservedHistory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ObservedFact {
    #[serde(default)]
    pub attribute: String,
    #[serde(default)]
    pub value: String,
    #[serde(default, alias = "asOf")]
    pub as_of: String,
    #[serde(default)]
    pub status: FactState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum FactState {
    #[default]
    Current,
    Ended,
}

impl FactState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Ended => "ended",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Self {
        if text.eq_ignore_ascii_case("ended") {
            Self::Ended
        } else {
            Self::Current
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ObservedHistory {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            customserde,
            reason = "the model sends a uuid, a string, or nothing for one field"
        )
    )]
    #[serde(default, alias = "summaryId", deserialize_with = "lenient_uuid")]
    #[schemars(with = "Option<String>")]
    pub summary_id: Option<Uuid>,
    #[serde(default)]
    pub sentence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct EntityProse {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub article: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct EntityIdentifications {
    #[serde(default)]
    pub observations: Vec<EntityIdentification>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct EntityIdentification {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            customserde,
            reason = "the model sends a uuid, a string, or nothing for one field"
        )
    )]
    #[serde(default, alias = "entityRef", deserialize_with = "lenient_uuid")]
    #[schemars(with = "Option<String>")]
    pub entity_ref: Option<Uuid>,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type", alias = "typeName")]
    pub type_name: String,
    #[serde(default)]
    pub aliases: Vec<String>,

    #[serde(default)]
    pub mention: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct EntityRewrite {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub article: String,
    #[serde(default, alias = "currentFacts")]
    pub current_facts: Vec<RewriteFact>,
    #[serde(default, alias = "historySentence")]
    pub history_sentence: String,

    #[serde(default)]
    pub retired: Vec<RetiredFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct RewriteFact {
    #[serde(default)]
    pub attribute: String,
    #[serde(default)]
    pub value: String,
    #[serde(default, alias = "asOf")]
    pub as_of: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct RetiredFact {
    #[serde(default)]
    pub attribute: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub detail: String,
}

impl RetiredFact {
    #[must_use]
    pub fn reason(&self) -> String {
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                dispatch,
                reason = "the model sends a free-text tag, checked against the known words"
            )
        )]
        match self.kind.as_str() {
            RENAMED => format!("renamed to {}", self.detail.trim()),
            CONTRADICTED => format!("contradicted: {}", self.detail.trim()),
            _ => String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedRetirement {
    pub attribute: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactReconciliation {
    pub facts: Vec<PreparedFact>,
    pub history_sentence: String,
    pub carried: Vec<PreparedFact>,
    pub retired: Vec<RetiredFact>,
    pub rejected: Vec<RejectedRetirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EntityPage {
    pub summary: String,
    pub article: String,
    pub facts: Vec<LiveFact>,

    pub observed_types: Vec<String>,

    pub fixed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRewrite {
    pub summary: String,
    pub article: String,
    pub facts: Vec<PreparedFact>,
    pub history: Option<PreparedHistory>,
    pub owner: String,

    pub session_at: DateTime<Utc>,

    pub session_id: Uuid,

    pub evidence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedMark {
    pub at: DateTime<Utc>,
    pub summary_id: Uuid,
}

#[must_use]
pub fn older_than_mark(session_at: DateTime<Utc>, session_id: Uuid, mark: AppliedMark) -> bool {
    (session_at, session_id) < (mark.at, mark.summary_id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEntity {
    pub entity_ref: Option<Uuid>,
    pub name: String,
    pub type_name: String,
    pub aliases: Vec<String>,
    pub facts: Vec<PreparedFact>,
    pub history: Vec<PreparedHistory>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedFact {
    pub attribute: String,
    pub value: String,
    pub as_of: DateTime<Utc>,
    pub status: FactState,
    pub summary_id: Uuid,
    pub conversation_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedHistory {
    pub summary_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub session_on: NaiveDate,
    pub sentence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KnownFact {
    pub attribute: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KnownEntity {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub aliases: Vec<String>,
    pub facts: Vec<KnownFact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObserveContext {
    pub types: Vec<String>,
    pub known: Vec<KnownEntity>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateQuery {
    pub names: Vec<String>,
    pub type_name: String,
    pub embedding: Option<Vec<f32>>,
    pub markers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeBrief {
    pub name: String,
    pub slug: String,
    pub aliases: Vec<String>,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchDecision {
    pub kind: String,
    pub proposed_name: String,
    pub proposed_type: String,
    pub candidates: Vec<String>,
    pub verdict: String,
    pub choice: String,
    pub confidence: f32,
    pub model: String,
    pub entity_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchCandidate {
    pub id: Uuid,
    pub name: String,
    pub type_name: String,
    pub aliases: Vec<String>,
    pub summary: String,
    pub trigram: f32,
    pub embedding: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveFact {
    pub attribute: String,
    pub value: String,
    pub as_of: DateTime<Utc>,
    pub status: FactState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryLine {
    pub date: NaiveDate,
    pub sentence: String,
    pub summary_id: Uuid,
    pub conversation_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityView {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub type_name: String,
    pub type_slug: String,
    pub aliases: Vec<String>,
    pub facts: Vec<LiveFact>,
    pub history: Vec<HistoryLine>,
    pub snapshot: String,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    Stored { git_sha: String, slug: String },
    Stale(Box<EntityView>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlugOccupant {
    Free,
    Same,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindHit {
    pub id: Uuid,
    pub type_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindRequest {
    pub entity_ref: Option<Uuid>,
    pub ref_exists: bool,
    pub type_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactRecord {
    pub id: Uuid,
    pub entity: Uuid,
    pub attribute: String,
    pub value: String,
    pub as_of: DateTime<Utc>,
    pub summary: Uuid,
    pub superseded_by: Option<Uuid>,
}

#[derive(Debug, Error)]
pub enum EntityStoreError {
    #[error("wiki directory is not configured")]
    DirectoryMissing,
    #[error("entity page write failed: {0}")]
    Page(#[from] WikiWriteError),
    #[error("entity store failed: {0}")]
    Query(#[source] BoxError),
    #[error("summary {0} is missing")]
    Missing(Uuid),
    #[error("entity slug is taken: {0}")]
    SlugTaken(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityMerge {
    pub survivor: Uuid,
    pub absorbed: Uuid,
    pub survivor_name: String,
    pub absorbed_name: String,

    pub performed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionNote {
    pub history: Option<PreparedHistory>,
    pub session_at: DateTime<Utc>,
    pub session_id: Uuid,
    pub owner: String,
}
