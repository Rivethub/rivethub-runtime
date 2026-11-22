use std::str::FromStr;

use async_trait::async_trait;
use jiff::Timestamp;
use strum::IntoEnumIterator;
use thiserror::Error;

use super::JobQueueError;
use super::WorkerRole;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, strum::Display, strum::EnumIter,
)]
#[strum(serialize_all = "kebab-case")]
pub enum PausableRole {
    Categorizer,
    WikiBuilder,
    EntityBuilder,
}

impl PausableRole {
    #[must_use]
    pub const fn worker_role(self) -> WorkerRole {
        match self {
            Self::Categorizer => WorkerRole::Categorizer,
            Self::WikiBuilder => WorkerRole::WikiBuilder,
            Self::EntityBuilder => WorkerRole::EntityBuilder,
        }
    }

    #[must_use]
    pub const fn of(role: WorkerRole) -> Option<Self> {
        match role {
            WorkerRole::Categorizer => Some(Self::Categorizer),
            WorkerRole::WikiBuilder => Some(Self::WikiBuilder),
            WorkerRole::EntityBuilder => Some(Self::EntityBuilder),
            WorkerRole::Embedder | WorkerRole::Summarizer => None,
        }
    }
}

impl FromStr for PausableRole {
    type Err = Unpausable;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::iter()
            .find(|role| role.to_string() == text)
            .ok_or_else(|| Unpausable {
                given: text.to_owned(),
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{given} cannot be paused; the pausable roles are {}", pausable())]
pub struct Unpausable {
    pub given: String,
}

fn pausable() -> String {
    PausableRole::iter()
        .map(|role| role.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolePause {
    pub role: PausableRole,
    pub paused_at: Timestamp,
    pub paused_by: Option<String>,
}

#[async_trait]
pub trait WorkerPauses: Send + Sync {
    async fn paused(&self) -> Result<Vec<RolePause>, JobQueueError>;

    async fn pause(&self, role: PausableRole, by: &str) -> Result<RolePause, JobQueueError>;

    async fn resume(&self, role: PausableRole) -> Result<(), JobQueueError>;
}
