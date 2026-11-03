mod apply;
mod edit;
mod guard;
mod proposal;
mod upkeep;

use std::collections::BTreeSet;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use thiserror::Error;

use crate::BoxError;
use crate::topics::TopicError;

pub use apply::BatchOutcome;
pub use apply::Decision;
pub use apply::Skipped;
pub use apply::TidyPorts;
pub use apply::approve;
pub use apply::decide_batch;
pub use apply::reject;
pub use edit::EditRecord;
pub use edit::TreeEdit;
pub use guard::Cluster;
pub use guard::Guard;
pub use guard::MIN_SESSIONS;
pub use guard::Refused;
pub use guard::clusters;
pub use guard::duplicates;
pub use proposal::Batch;
pub use proposal::BatchId;
pub use proposal::Evidence;
pub use proposal::Fingerprint;
pub use proposal::Proposal;
pub use proposal::ProposalId;
pub use proposal::ProposalState;
pub use proposal::Proposer;
pub use proposal::Reason;
pub use proposal::RecentFiling;
pub use proposal::Sample;
pub use proposal::Suggested;
pub use proposal::TidyInputs;
pub use upkeep::FilingTally;
pub use upkeep::RoleActivity;
pub use upkeep::TaskActivity;
pub use upkeep::TypeTally;
pub use upkeep::WikiTally;

pub const WEEK: TimeDelta = TimeDelta::days(7);

#[derive(Debug, Error)]
pub enum TidyError {
    #[error("unknown proposal {id}")]
    UnknownProposal { id: ProposalId },
    #[error("unknown batch {id}")]
    UnknownBatch { id: BatchId },
    #[error("proposal {id} is already {state}")]
    Decided {
        id: ProposalId,
        state: ProposalState,
    },
    #[error("a proposal needs a reason")]
    EmptyReason,
    #[error("a stored proposal names \"{what}\", which is not an id")]
    Invalid { what: String },
    #[error(transparent)]
    Topic(#[from] TopicError),
    #[error(transparent)]
    Wiki(#[from] crate::wiki::builder::WikiStoreError),
    #[error("proposal {id} is out of date and was set aside: {reason}")]
    Stale { id: ProposalId, reason: String },
    #[error("wiki pages are not configured")]
    PagesUnavailable,
    #[error("tidy store is unavailable")]
    Unavailable(#[source] BoxError),
}

#[async_trait]
pub trait TidyStore: Send + Sync {
    async fn propose(&self, batch: Batch) -> Result<Batch, TidyError>;

    async fn pending(&self) -> Result<Vec<Batch>, TidyError>;

    async fn proposal(&self, id: ProposalId) -> Result<Option<Proposal>, TidyError>;

    async fn batch(&self, id: BatchId) -> Result<Option<Batch>, TidyError>;

    async fn decide(
        &self,
        id: ProposalId,
        state: ProposalState,
        at: DateTime<Utc>,
    ) -> Result<Proposal, TidyError>;

    async fn last_batch(&self, by: Proposer) -> Result<Option<DateTime<Utc>>, TidyError>;

    async fn known(&self) -> Result<BTreeSet<Fingerprint>, TidyError>;

    async fn inputs(&self, since: DateTime<Utc>) -> Result<TidyInputs, TidyError>;
}

#[async_trait]
pub trait UpkeepStore: Send + Sync {
    async fn filing(&self, since: DateTime<Utc>) -> Result<FilingTally, TidyError>;

    async fn wiki(&self) -> Result<WikiTally, TidyError>;

    async fn tasks(&self) -> Result<Vec<TaskActivity>, TidyError>;
}
