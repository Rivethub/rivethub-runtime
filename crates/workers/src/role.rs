use std::str::FromStr;

use kernel::queue::FixedJob;
use kernel::queue::JobName;
use kernel::queue::WorkerRole;
use strum::EnumIter;
use strum::IntoEnumIterator;

use crate::error::WorkerError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::EnumString, EnumIter)]
#[strum(serialize_all = "kebab-case")]
pub enum ProcessRole {
    Embedder,
    Summarizer,
    Categorizer,
    WikiBuilder,
    EntityBuilder,
    All,
}

impl ProcessRole {
    pub fn parse(text: &str) -> Result<Self, WorkerError> {
        Self::from_str(text).map_err(|_| WorkerError::UnknownRole {
            text: text.to_owned(),
            expected: allowed_roles(),
        })
    }

    pub fn selected(self) -> Vec<WorkerRole> {
        match self {
            Self::Embedder => vec![WorkerRole::Embedder],
            Self::Summarizer => vec![WorkerRole::Summarizer],
            Self::Categorizer => vec![WorkerRole::Categorizer],
            Self::WikiBuilder => vec![WorkerRole::WikiBuilder],
            Self::EntityBuilder => vec![WorkerRole::EntityBuilder],
            Self::All => vec![
                WorkerRole::Embedder,
                WorkerRole::Summarizer,
                WorkerRole::Categorizer,
                WorkerRole::WikiBuilder,
                WorkerRole::EntityBuilder,
            ],
        }
    }

    pub fn claim_roles(self) -> Result<Vec<WorkerRole>, WorkerError> {
        let runnable = self.selected();
        if runnable.iter().any(|role| !role_has_handler(*role)) {
            return Err(WorkerError::RoleUnavailable { role: self });
        }
        Ok(runnable)
    }
}

pub fn jobs_for(role: WorkerRole) -> Vec<JobName> {
    role.task_list().to_vec()
}

pub fn accepts(role: WorkerRole, name: &JobName) -> bool {
    jobs_for(role).iter().any(|job| job == name)
}

fn role_has_handler(role: WorkerRole) -> bool {
    jobs_for(role).iter().all(handler_ready)
}

pub(crate) fn handler_ready(name: &JobName) -> bool {
    matches!(
        name,
        JobName::Fixed(
            FixedJob::EmbedTarget
                | FixedJob::SynthesizeToolCall
                | FixedJob::SummarizeConversation
                | FixedJob::EnqueueIdle
                | FixedJob::EnqueueStaleSummaries
                | FixedJob::ReapDeadJobs
                | FixedJob::FileConversation
                | FixedJob::ProposeTidy
                | FixedJob::ExtractWiki
                | FixedJob::EnqueueWikiBackfill
                | FixedJob::ConsolidateWiki
                | FixedJob::RecompileWiki
                | FixedJob::EnqueueUnembedded
                | FixedJob::EnqueueUnchunked
                | FixedJob::EnqueueStaleWiki
                | FixedJob::SyncTopicPage
                | FixedJob::PurgeDeletedSessions
                | FixedJob::DigestDay
                | FixedJob::ExtractEntities
                | FixedJob::EnqueueEntityBackfill
                | FixedJob::EnqueueStaleEntities
        )
    )
}

fn allowed_roles() -> String {
    ProcessRole::iter()
        .map(|role| role.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
