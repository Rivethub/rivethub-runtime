use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::session_id::SessionId;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "lowercase")]
pub enum ProposalState {
    Pending,
    Approved,
    Rejected,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "lowercase")]
pub enum Proposer {
    Categorizer,
    Backfill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TidyChange {
    Merge {
        source: String,
        into: String,
    },
    Move {
        topic: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },
    Rename {
        topic: String,
        name: String,
    },
    Close {
        topic: String,
    },
    Delete {
        topic: String,
    },
    CreateTopic {
        topic: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        aliases: Vec<String>,
        #[serde(default)]
        sessions: Vec<SessionId>,
        #[serde(default)]
        conversations: Vec<String>,
    },
    FileInto {
        topic: String,
        #[serde(default)]
        sessions: Vec<SessionId>,
        #[serde(default)]
        conversations: Vec<String>,
    },
    MergePage {
        page: String,
        into: String,
    },
    RemovePage {
        page: String,
    },
    AddType {
        topic: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TidySample {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
    #[serde(default)]
    pub last_active: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TidyEvidence {
    pub sessions: u64,
    #[serde(default)]
    pub samples: Vec<TidySample>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TidyProposal {
    pub id: String,
    pub batch_id: String,
    pub change: TidyChange,
    pub reason: String,
    pub evidence: TidyEvidence,
    pub proposed_by: Proposer,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub proposed_at: DateTime<Utc>,
    pub state: ProposalState,
    #[serde(default)]
    #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
    pub decided_at: Option<DateTime<Utc>>,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TidyBatch {
    pub batch_id: String,
    pub proposed_by: Proposer,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub proposed_at: DateTime<Utc>,
    pub proposals: Vec<TidyProposal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TidyPending {
    pub batches: Vec<TidyBatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TidyFailure {
    pub proposal_id: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TidySkipped {
    pub proposal_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TidyBatchDecision {
    pub decided: Vec<TidyProposal>,
    #[serde(default)]
    pub skipped: Vec<TidySkipped>,
    #[serde(default)]
    pub failed: Vec<TidyFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FilingUpkeep {
    pub categorizer: u64,
    pub you: u64,
    pub uncategorized: u64,
    pub left_out: u64,
    pub corrections_last_week: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EntityTypeCount {
    pub type_id: String,
    pub name: String,
    pub entities: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiUpkeep {
    pub topic_pages: u64,
    pub entity_types: Vec<EntityTypeCount>,
    pub loose_pages: u64,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkerActivity {
    #[serde(default)]
    #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
    pub last_finished_at: Option<DateTime<Utc>>,
    pub failing: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkerUpkeep {
    pub embedder: WorkerActivity,
    pub summarizer: WorkerActivity,
    pub categorizer: WorkerActivity,
    pub wiki_builder: WorkerActivity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryUpkeep {
    pub filing: FilingUpkeep,
    pub wiki: WikiUpkeep,
    pub workers: WorkerUpkeep,
    pub pending_proposals: u64,
}
