use jiff::Timestamp;
use std::fmt;
use std::str::FromStr;

use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;
use values::SessionId;

use super::TidyError;
use super::TreeEdit;
use crate::topics::TopicId;
use crate::topics::TopicName;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::EnumString)]
#[strum(serialize_all = "lowercase")]
pub enum ProposalState {
    Pending,
    Approved,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::EnumString)]
#[strum(serialize_all = "lowercase")]
pub enum Proposer {
    Categorizer,
    Backfill,
}

#[serde_with::skip_serializing_none]
#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    #[serde_as(as = "Option<values::time::Rfc3339>")]
    pub last_active: Option<Timestamp>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub sessions: u64,
    #[serde(default)]
    pub samples: Vec<Sample>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProposalId(Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BatchId(Uuid);

impl ProposalId {
    #[must_use]
    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn uuid(self) -> Uuid {
        self.0
    }
}

impl BatchId {
    #[must_use]
    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for ProposalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for BatchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for ProposalId {
    type Err = uuid::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(text).map(Self)
    }
}

impl FromStr for BatchId {
    type Err = uuid::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(text).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint(String);

impl Fingerprint {
    pub(super) fn of(parts: &[&str]) -> Self {
        Self(parts.join(":"))
    }

    #[must_use]
    pub fn stored(text: String) -> Self {
        Self(text)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason(String);

impl Reason {
    pub fn new(text: &str) -> Result<Self, TidyError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            Err(TidyError::EmptyReason)
        } else {
            Ok(Self(trimmed.to_owned()))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub id: ProposalId,
    pub batch: BatchId,
    pub by: Proposer,
    pub proposed_at: Timestamp,
    pub change: TreeEdit,
    pub reason: Reason,
    pub evidence: Evidence,
    pub state: ProposalState,
    pub decided_at: Option<Timestamp>,
}

impl Proposal {
    pub fn pending(&self) -> Result<(), TidyError> {
        match self.state {
            ProposalState::Pending => Ok(()),
            ProposalState::Approved | ProposalState::Rejected => Err(TidyError::Decided {
                id: self.id,
                state: self.state,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub id: BatchId,
    pub by: Proposer,
    pub at: Timestamp,
    pub proposals: Vec<Proposal>,
}

impl Batch {
    #[must_use]
    pub fn new(by: Proposer, at: Timestamp) -> Self {
        Self {
            id: BatchId(Uuid::new_v4()),
            by,
            at,
            proposals: Vec::new(),
        }
    }

    pub fn add(&mut self, change: TreeEdit, reason: Reason, evidence: Evidence) {
        self.proposals.push(Proposal {
            id: ProposalId(Uuid::new_v4()),
            batch: self.id,
            by: self.by,
            proposed_at: self.at,
            change,
            reason,
            evidence,
            state: ProposalState::Pending,
            decided_at: None,
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggested {
    pub session: SessionId,
    pub name: TopicName,
    pub parent: Option<TopicId>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentFiling {
    pub topic: TopicId,
    pub count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TidyInputs {
    pub suggestions: Vec<Suggested>,
    pub recent: Vec<RecentFiling>,
}
