use crate::queue::{FixedJob, Job, JobKeyMode, JobName, MAINTENANCE_PRIORITY, WorkerRole};
use values::JsonObject;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronEntry {
    pub identifier: String,
    pub task: JobName,
    pub schedule: String,
    pub backfill_seconds: u64,
    pub max_attempts: i32,
    pub every_minutes: u32,
}

impl CronEntry {
    #[must_use]
    pub fn job(&self) -> Job {
        Job {
            name: self.task.clone(),
            payload: JsonObject::default(),
            key: Some(self.identifier.clone()),
            key_mode: JobKeyMode::KeepRunAt,
            priority: MAINTENANCE_PRIORITY,
            run_at: None,
            max_attempts: Some(self.max_attempts),
        }
    }
}

impl WorkerRole {
    #[must_use]
    pub fn crontab(self, wiki_enabled: bool) -> Vec<CronEntry> {
        match self {
            Self::Embedder => vec![
                cron("unembedded-sweep", FixedJob::EnqueueUnembedded, 10),
                cron("unchunked-sweep", FixedJob::EnqueueUnchunked, 15),
            ],
            Self::Summarizer => vec![
                cron("idle-enqueue", FixedJob::EnqueueIdle, 5),
                cron(
                    "stale-compaction-sweep",
                    FixedJob::EnqueueStaleSummaries,
                    15,
                ),
                cron("reap-dead-jobs", FixedJob::ReapDeadJobs, 60),
                cron("topic-page-sweep", FixedJob::SyncTopicPage, 60),
                cron("deleted-session-sweep", FixedJob::PurgeDeletedSessions, 15),
            ],
            Self::Categorizer => vec![cron("tidy-sweep", FixedJob::ProposeTidy, 60)],
            Self::WikiBuilder => {
                let mut entries = vec![cron("wiki-backfill", FixedJob::EnqueueWikiBackfill, 1)];
                if wiki_enabled {
                    entries.push(cron("stale-wiki-sweep", FixedJob::EnqueueStaleWiki, 15));
                }
                entries
            }
            Self::EntityBuilder => {
                let mut entries = vec![cron("entity-backfill", FixedJob::EnqueueEntityBackfill, 1)];
                if wiki_enabled {
                    entries.push(cron(
                        "stale-entity-sweep",
                        FixedJob::EnqueueStaleEntities,
                        15,
                    ));
                }
                entries
            }
        }
    }
}

fn cron(identifier: &str, task: FixedJob, every_minutes: u32) -> CronEntry {
    CronEntry {
        identifier: identifier.to_owned(),
        task: JobName::Fixed(task),
        schedule: if every_minutes == 60 {
            "0 * * * *".to_owned()
        } else {
            format!("*/{every_minutes} * * * *")
        },
        backfill_seconds: 0,
        max_attempts: 25,
        every_minutes,
    }
}
