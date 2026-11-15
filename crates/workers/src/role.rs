use kernel::queue::JobName;
use kernel::queue::WorkerRole;
use strum::EnumIter;

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
}

pub fn jobs_for(role: WorkerRole) -> Vec<JobName> {
    role.task_list().to_vec()
}
