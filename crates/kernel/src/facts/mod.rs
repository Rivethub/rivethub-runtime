mod fields;
mod state;

use ::values::SessionId;
use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use thiserror::Error;
use uuid::Uuid;

use crate::BoxError;
use crate::memory::Provenance;
use crate::owner::Owner;
use crate::topics::TopicId;

pub use self::fields::Evidence;
pub use self::fields::FactId;
pub use self::fields::FactKey;
pub use self::fields::FactValue;
pub use self::state::CONFLICT_WINDOW;
pub use self::state::FactChange;
pub use self::state::FactEvent;
pub use self::state::FactState;
pub use self::state::Previous;
pub use self::state::Support;
pub use self::state::connected;
pub use self::state::facts_as_of;
pub use self::state::timeline;

#[derive(Debug, Error)]
pub enum FactError {
    #[error("A fact needs a key with a letter or digit.")]
    EmptyKey,
    #[error("A fact needs a value.")]
    EmptyValue,
    #[error("A fact's {field} is at most {max} characters.")]
    TooLong { field: &'static str, max: usize },
    #[error("fact store is unavailable")]
    Unavailable(#[source] BoxError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    You {
        by: Owner,
        pinned: bool,
    },
    Writer {
        session: SessionId,
        summary: Uuid,
        evidence: Option<Evidence>,
        settled: bool,
    },
}

impl Said {
    #[must_use]
    pub const fn pinned(&self) -> bool {
        matches!(self, Self::You { pinned: true, .. })
    }

    #[must_use]
    pub const fn settled(&self) -> bool {
        matches!(self, Self::Writer { settled: true, .. })
    }

    #[must_use]
    pub const fn session(&self) -> Option<&SessionId> {
        match self {
            Self::You { .. } => None,
            Self::Writer { session, .. } => Some(session),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewClaim {
    pub entity: TopicId,
    pub key: FactKey,
    pub value: FactValue,
    pub said: Said,
    pub owner: Option<Owner>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub fact: FactId,
    pub key: FactKey,
    pub value: FactValue,
    pub said: Said,
    pub at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionLink {
    pub entity: TopicId,
    pub session: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityHistory {
    pub at: DateTime<Utc>,
    pub sentence: String,
    pub session: Option<SessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactSession {
    pub session: SessionId,
    pub title: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub messages: u64,
    pub provenance: Option<Provenance>,
}

#[async_trait]
pub trait FactStore: Send + Sync {
    async fn assert_facts(&self, claims: &[NewClaim]) -> Result<(), FactError>;

    async fn claims(&self, entity: TopicId) -> Result<Vec<Claim>, FactError>;

    async fn session_links(&self) -> Result<Vec<SessionLink>, FactError>;

    async fn fact_sessions(&self, sessions: &[SessionId]) -> Result<Vec<FactSession>, FactError>;

    async fn entity_history(&self, entity: TopicId) -> Result<Vec<EntityHistory>, FactError>;
}
