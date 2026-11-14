use std::collections::BTreeMap;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use kernel::queue::ClaimSelector;
use kernel::queue::ClaimedJob;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobId;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::queue::JobQueueError;
use kernel::queue::StoredJob;
use kernel::queue::WorkerRole;
use tokio::sync::Mutex;

#[derive(Debug)]
struct Held {
    job: Job,
    attempts: u32,
    last_error: Option<String>,
    claimed: bool,
}

#[derive(Debug, Default)]
pub struct FakeJobQueue {
    jobs: Mutex<BTreeMap<JobId, Held>>,
    next_id: AtomicI64,
    notifications: tokio::sync::watch::Sender<()>,
}

#[async_trait]
impl JobQueue for FakeJobQueue {
    fn task_names(&self, role: WorkerRole) -> Vec<JobName> {
        role.task_list().to_vec()
    }

    async fn subscribe(&self) -> Result<Box<dyn kernel::queue::JobNotifications>, JobQueueError> {
        Ok(Box::new(self.notifications.subscribe()))
    }

    async fn enqueue(&self, job: Job) -> Result<JobId, JobQueueError> {
        let mut jobs = self.jobs.lock().await;
        let replaced = job.key.as_ref().and_then(|key| {
            jobs.iter()
                .find(|(_, held)| held.job.key.as_ref() == Some(key))
                .map(|(id, _)| *id)
        });
        let id = if let Some(id) = replaced {
            let mode = job.key_mode;
            let Some(held) = jobs.get_mut(&id) else {
                return Err(JobQueueError::Missing { id });
            };
            let claimed = held.claimed;
            held.job = job;
            if mode == kernel::queue::JobKeyMode::Replace && !claimed {
                held.attempts = 0;
                held.last_error = None;
            }
            id
        } else {
            let id = JobId::new(self.next_id.fetch_add(1, Ordering::Relaxed));
            jobs.insert(
                id,
                Held {
                    job,
                    attempts: 0,
                    last_error: None,
                    claimed: false,
                },
            );
            id
        };
        drop(jobs);
        self.notifications.send_replace(());
        Ok(id)
    }

    async fn claim(&self, selector: &ClaimSelector) -> Result<Option<ClaimedJob>, JobQueueError> {
        let mut jobs = self.jobs.lock().await;
        let outcome = match jobs.iter().find_map(|(id, held)| {
            let named = selector.names.iter().any(|name| name == &held.job.name);
            let pinned = selector.only_id.is_none_or(|only| only == *id);
            (named && pinned && !held.claimed).then_some(*id)
        }) {
            None => Ok(None),
            Some(id) => match jobs.get_mut(&id) {
                None => Err(JobQueueError::Missing { id }),
                Some(held) => {
                    held.claimed = true;
                    held.attempts = held.attempts.saturating_add(1);
                    Ok(Some(ClaimedJob {
                        id,
                        name: held.job.name.clone(),
                        payload: held.job.payload.clone(),
                        attempt: held.attempts,
                    }))
                }
            },
        };
        drop(jobs);
        outcome
    }

    async fn complete(&self, id: JobId) -> Result<(), JobQueueError> {
        let mut jobs = self.jobs.lock().await;
        let missing = jobs.remove(&id).is_none();
        drop(jobs);
        if missing {
            Err(JobQueueError::Missing { id })
        } else {
            Ok(())
        }
    }

    async fn release(&self, id: JobId) -> Result<(), JobQueueError> {
        let mut jobs = self.jobs.lock().await;
        let outcome = match jobs.get_mut(&id) {
            Some(held) if held.claimed => {
                held.claimed = false;
                held.attempts = held.attempts.saturating_sub(1);
                Ok(())
            }
            Some(_) | None => Err(JobQueueError::Missing { id }),
        };
        drop(jobs);
        outcome
    }

    async fn fail(&self, id: JobId, error: &str) -> Result<(), JobQueueError> {
        let mut jobs = self.jobs.lock().await;
        let outcome = match jobs.get_mut(&id) {
            Some(held) => {
                held.last_error = Some(error.to_owned());
                held.claimed = false;
                Ok(())
            }
            None => Err(JobQueueError::Missing { id }),
        };
        drop(jobs);
        outcome
    }

    async fn flag_stuck(&self) -> Result<u64, JobQueueError> {
        let mut jobs = self.jobs.lock().await;
        let before = jobs.len();
        jobs.retain(|_, held| {
            held.job
                .max_attempts
                .is_none_or(|max| i64::from(held.attempts) < i64::from(max))
        });
        let flagged = before - jobs.len();
        drop(jobs);
        u64::try_from(flagged).map_err(|_| JobQueueError::Stored("flagged".to_owned()))
    }

    async fn read(&self, id: JobId) -> Result<Option<StoredJob>, JobQueueError> {
        let jobs = self.jobs.lock().await;
        let stored = jobs.get(&id).map(|held| StoredJob {
            id,
            name: held.job.name.clone(),
            payload: held.job.payload.clone(),
            key: held.job.key.clone(),
            priority: Some(held.job.priority),
            max_attempts: held
                .job
                .max_attempts
                .unwrap_or_else(|| ceiling(&held.job.name)),
            attempts: held.attempts,
            last_error: held.last_error.clone(),
        });
        drop(jobs);
        Ok(stored)
    }
}

fn ceiling(name: &JobName) -> i32 {
    match name {
        JobName::Fixed(fixed) => match fixed {
            FixedJob::EmbedTarget | FixedJob::SyncTopicPage => 5,
            FixedJob::SummarizeConversation
            | FixedJob::SynthesizeToolCall
            | FixedJob::DigestDay => 3,
            FixedJob::FileConversation | FixedJob::ExtractWiki | FixedJob::ExtractEntities => 2,
            FixedJob::EnqueueUnembedded
            | FixedJob::EnqueueUnchunked
            | FixedJob::EnqueueIdle
            | FixedJob::EnqueueWikiBackfill
            | FixedJob::ConsolidateWiki
            | FixedJob::RecompileWiki
            | FixedJob::EnqueueStaleWiki
            | FixedJob::EnqueueStaleSummaries
            | FixedJob::EnqueueEntityBackfill
            | FixedJob::EnqueueStaleEntities
            | FixedJob::ReapDeadJobs
            | FixedJob::ProposeTidy
            | FixedJob::PurgeDeletedSessions => 25,
        },
        JobName::Heartbeat { .. } => 1,
    }
}
