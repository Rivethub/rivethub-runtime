use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use kernel::memory::EmbeddingStore;
use kernel::memory::SummaryStore;
use kernel::model::ModelProvider;
use kernel::queue::ClaimedJob;
use kernel::queue::JobId;
use kernel::queue::JobNotifications;
use kernel::queue::JobQueue;
use kernel::queue::WorkerRole;
use kernel::shutdown::Activities;
use kernel::topics::DecisionStore;
use kernel::topics::TopicPageStore;
use kernel::topics::TopicStore;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::dispatch::JobEnv;
use crate::embed::EmbedSettings;
use crate::error::WorkerError;
use crate::outage::OutageGate;
use crate::role::ProcessRole;
use crate::settle::settle;
use crate::summarize::SummarySettings;
use crate::wiki_builder::WikiBuilderDeps;

pub const EMBED_CONCURRENCY: u32 = 4;
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerConfig {
    pub role: ProcessRole,
    pub concurrency: u32,
    pub poll_interval: Duration,
}

impl RunnerConfig {
    pub fn new(role: ProcessRole, concurrency: u32) -> Result<Self, WorkerError> {
        if concurrency == 0 {
            return Err(WorkerError::Concurrency);
        }
        Ok(Self {
            role,
            concurrency,
            poll_interval: POLL_INTERVAL,
        })
    }

    #[must_use]
    pub fn embedder() -> Self {
        Self {
            role: ProcessRole::Embedder,
            concurrency: EMBED_CONCURRENCY,
            poll_interval: POLL_INTERVAL,
        }
    }
}

#[expect(
    missing_debug_implementations,
    reason = "the model provider and the summary and embedding stores are trait objects"
)]
pub struct WorkerDeps {
    pub targets: Arc<dyn EmbeddingStore>,
    pub summaries: Arc<dyn SummaryStore>,
    pub provider: Arc<dyn ModelProvider>,
    pub embed: EmbedSettings,
    pub summary: SummarySettings,
    pub scopes: Option<crate::OwnerScopes>,
}

#[expect(
    missing_debug_implementations,
    reason = "the model provider and the summary and embedding stores are trait objects"
)]
pub struct Runner<Q> {
    job_settings: crate::MemoryJobSettings,
    schedules: bool,
    schedule_at_start: bool,
    wiki_enabled: bool,
    memory_jobs: Option<Arc<dyn kernel::memory::jobs::MemoryJobStore>>,
    unembedded: Option<Arc<dyn kernel::memory::jobs::UnembeddedStore>>,
    wiki: Option<WikiBuilderDeps>,
    topics: Option<crate::SessionTopics>,
    topic_pages: Option<crate::topic_page::TopicPages>,
    tidy: Option<Arc<dyn kernel::tidy::TidyStore>>,
    deletions: Option<Arc<dyn kernel::memory::SessionDeletion>>,
    scopes: Option<crate::OwnerScopes>,
    queue: Arc<Q>,
    targets: Arc<dyn EmbeddingStore>,
    summaries: Arc<dyn SummaryStore>,
    provider: Arc<dyn ModelProvider>,
    config: RunnerConfig,
    embed: EmbedSettings,
    summary: SummarySettings,
    wake: Arc<Notify>,
    outage: Arc<OutageGate>,
    idle: watch::Sender<bool>,
    cursor: Arc<AtomicUsize>,
    open: Arc<Mutex<Vec<JobId>>>,
    activities: Activities,
}

impl<Q> Runner<Q>
where
    Q: JobQueue + 'static,
{
    #[must_use]
    pub fn new(queue: Arc<Q>, config: RunnerConfig, deps: WorkerDeps) -> Self {
        let (idle, _) = watch::channel(false);
        Self {
            job_settings: crate::MemoryJobSettings::standard(),
            schedules: false,
            schedule_at_start: false,
            wiki_enabled: true,
            memory_jobs: None,
            unembedded: None,
            wiki: None,
            topics: None,
            topic_pages: None,
            tidy: None,
            deletions: None,
            scopes: deps.scopes,
            queue,
            targets: deps.targets,
            summaries: deps.summaries,
            provider: deps.provider,
            config,
            embed: deps.embed,
            summary: deps.summary,
            wake: Arc::new(Notify::new()),
            outage: Arc::new(OutageGate::new()),
            idle,
            cursor: Arc::new(AtomicUsize::new(0)),
            open: Arc::new(Mutex::new(Vec::new())),
            activities: Activities::new(),
        }
    }

    pub fn tuned(mut self, env: &config::Environment) -> Result<Self, WorkerError> {
        self.summary = crate::tuned_summary(self.summary, env)?;
        self.embed = crate::tuned_embed(self.embed, env)?;
        Ok(self)
    }

    #[must_use]
    pub fn with_concurrency(mut self, concurrency: u32) -> Self {
        self.config.concurrency = concurrency;
        self
    }

    #[must_use]
    pub fn with_activities(mut self, activities: Activities) -> Self {
        self.activities = activities;
        self
    }

    #[must_use]
    pub fn with_job_settings(mut self, settings: crate::MemoryJobSettings) -> Self {
        self.job_settings = settings;
        self
    }

    #[must_use]
    pub fn with_schedules(mut self, wiki_enabled: bool) -> Self {
        self.schedules = true;
        self.wiki_enabled = wiki_enabled;
        self
    }

    #[must_use]
    pub fn with_schedule_at_start(mut self) -> Self {
        self.schedule_at_start = true;
        self
    }

    #[must_use]
    pub fn with_memory_jobs(
        mut self,
        store: Arc<dyn kernel::memory::jobs::MemoryJobStore>,
    ) -> Self {
        self.memory_jobs = Some(store);
        self
    }

    #[must_use]
    pub fn with_unembedded(
        mut self,
        store: Arc<dyn kernel::memory::jobs::UnembeddedStore>,
    ) -> Self {
        self.unembedded = Some(store);
        self
    }

    #[must_use]
    pub fn with_wiki_builder(mut self, deps: WikiBuilderDeps) -> Self {
        self.wiki = Some(deps);
        self
    }

    #[must_use]
    pub fn with_session_deletion(
        mut self,
        deletions: Arc<dyn kernel::memory::SessionDeletion>,
    ) -> Self {
        self.deletions = Some(deletions);
        self
    }

    #[must_use]
    pub fn with_tidy(mut self, tidy: Arc<dyn kernel::tidy::TidyStore>) -> Self {
        self.tidy = Some(tidy);
        self
    }

    #[must_use]
    pub fn with_session_topics(mut self, topics: crate::SessionTopics) -> Self {
        self.topics = Some(topics);
        self
    }

    #[must_use]
    pub fn with_topic_pages(
        mut self,
        topics: Arc<dyn TopicStore>,
        pages: Arc<dyn TopicPageStore>,
        decisions: Arc<dyn DecisionStore>,
    ) -> Self {
        self.topic_pages = Some(crate::topic_page::TopicPages {
            topics,
            pages,
            decisions,
        });
        self
    }

    #[must_use]
    pub fn idle(&self) -> watch::Receiver<bool> {
        self.idle.subscribe()
    }

    pub async fn run(&self, cancel: CancellationToken) -> Result<(), WorkerError> {
        let roles = self.config.role.claim_roles()?;
        if self.wiki.is_none() && roles.contains(&WorkerRole::WikiBuilder) {
            return Err(WorkerError::MissingDependencies {
                role: ProcessRole::WikiBuilder,
            });
        }
        let permits =
            usize::try_from(self.config.concurrency).map_err(|_| WorkerError::Concurrency)?;
        let semaphore = Arc::new(Semaphore::new(permits));
        let cancel = cancel.child_token();
        let mut tasks = JoinSet::new();
        let listening = self.activities.enter("queue listen");
        let mut notifications = tokio::select! {
            biased;
            () = cancel.cancelled() => return Ok(()),
            result = self.queue.subscribe() => result.map_err(WorkerError::queue)?,
        };
        let claims = async {
            let claimed = self
                .claim_loop(
                    &roles,
                    &semaphore,
                    &mut tasks,
                    notifications.as_mut(),
                    &cancel,
                )
                .await;
            cancel.cancel();
            claimed
        };
        let schedule = async {
            if !self.schedules {
                return Ok(());
            }
            let scheduled = crate::schedule::run(
                self.queue.as_ref(),
                &roles,
                self.wiki_enabled,
                self.schedule_at_start,
                &cancel,
            )
            .await;
            cancel.cancel();
            scheduled
        };
        let (claimed, scheduled) = tokio::join!(claims, schedule);
        let outcome = claimed.and(scheduled);
        drop(notifications);
        drop(listening);
        cancel.cancel();
        let reaped = self.reap(&mut tasks, true).await;
        outcome.and(reaped)
    }

    async fn claim_loop(
        &self,
        roles: &[WorkerRole],
        semaphore: &Arc<Semaphore>,
        tasks: &mut JoinSet<Result<(), WorkerError>>,
        notifications: &mut dyn JobNotifications,
        cancel: &CancellationToken,
    ) -> Result<(), WorkerError> {
        loop {
            if cancel.is_cancelled() {
                return Ok(());
            }
            self.reap(tasks, false).await?;
            if self.outage.is_down() {
                self.publish_idle(tasks.is_empty());
                if !self.outage.wait_until_ready(cancel).await? {
                    return Ok(());
                }
                continue;
            }
            let Some(permit) = self.acquire(semaphore, cancel).await? else {
                return Ok(());
            };
            if self.outage.is_down() {
                drop(permit);
                self.publish_idle(tasks.is_empty());
                if !self.outage.wait_until_ready(cancel).await? {
                    return Ok(());
                }
                continue;
            }
            let job = match self.claim_one(roles, cancel).await {
                Ok(Some(job)) => job,
                Ok(None) => {
                    drop(permit);
                    self.publish_idle(tasks.is_empty());
                    if !self.park(notifications, cancel).await? {
                        return Ok(());
                    }
                    continue;
                }
                Err(error) if error.database_unreachable() => {
                    drop(permit);
                    self.publish_idle(tasks.is_empty());
                    if !wait_outage(notifications, cancel).await? {
                        return Ok(());
                    }
                    continue;
                }
                Err(error) => return Err(error),
            };
            self.note_open(job.id).await;
            self.spawn_job(tasks, permit, job, cancel.clone());
        }
    }

    async fn note_open(&self, id: JobId) {
        self.open.lock().await.push(id);
    }

    async fn acquire(
        &self,
        semaphore: &Arc<Semaphore>,
        cancel: &CancellationToken,
    ) -> Result<Option<OwnedSemaphorePermit>, WorkerError> {
        tokio::select! {
            biased;
            () = cancel.cancelled() => Ok(None),
            result = semaphore.clone().acquire_owned() => result.map(Some).map_err(|_| WorkerError::Closed),
        }
    }

    async fn claim_one(
        &self,
        roles: &[WorkerRole],
        cancel: &CancellationToken,
    ) -> Result<Option<ClaimedJob>, WorkerError> {
        let count = roles.len();
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % count;
        for role in roles.iter().cycle().skip(start).take(count) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            let selector = kernel::queue::ClaimSelector::matching(self.queue.task_names(*role));
            let claimed = self.queue.claim(&selector).await;
            if let Some(job) = claimed.map_err(WorkerError::queue)? {
                return Ok(Some(job));
            }
        }
        Ok(None)
    }

    fn spawn_job(
        &self,
        tasks: &mut JoinSet<Result<(), WorkerError>>,
        permit: OwnedSemaphorePermit,
        job: ClaimedJob,
        cancel: CancellationToken,
    ) {
        self.publish_idle(false);
        let queue = Arc::clone(&self.queue);
        let env = JobEnv {
            settings: self.job_settings,
            wiki_enabled: self.wiki_enabled,
            memory_jobs: self.memory_jobs.clone(),
            unembedded: self.unembedded.clone(),
            wiki: self.wiki.clone(),
            targets: Arc::clone(&self.targets),
            summaries: Arc::clone(&self.summaries),
            provider: Arc::clone(&self.provider),
            embed: self.embed,
            summary: self.summary.clone(),
            topics: self.topics.clone(),
            topic_pages: self.topic_pages.clone(),
            tidy: self.tidy.clone(),
            deletions: self.deletions.clone(),
            scopes: self.scopes.clone(),
        };
        let wake = Arc::clone(&self.wake);
        let open = Arc::clone(&self.open);
        let gate = Arc::clone(&self.outage);
        let id = job.id;
        let activities = self.activities.clone();
        let activity = format!("job {} {id}", job.name);
        tasks.spawn(kernel::tenancy::Scope::carry(async move {
            let _activity = activities.enter(activity);
            let result = settle(gate.as_ref(), queue.as_ref(), &env, job, &cancel).await;
            {
                let mut running = open.lock().await;
                running.retain(|seen| *seen != id);
            }
            drop(permit);
            wake.notify_one();
            result
        }));
    }

    async fn park(
        &self,
        notifications: &mut dyn JobNotifications,
        cancel: &CancellationToken,
    ) -> Result<bool, WorkerError> {
        tokio::select! {
            biased;
            () = cancel.cancelled() => Ok(false),
            notification = notifications.recv() => notification.map(|()| true).map_err(WorkerError::queue),
            () = self.wake.notified() => Ok(true),
            () = tokio::time::sleep(self.config.poll_interval) => Ok(true),
        }
    }

    fn publish_idle(&self, idle: bool) {
        let _previous = self.idle.send_replace(idle);
    }

    async fn reap(
        &self,
        tasks: &mut JoinSet<Result<(), WorkerError>>,
        wait: bool,
    ) -> Result<(), WorkerError> {
        let mut first = None;
        loop {
            let joined = if wait {
                tasks.join_next().await
            } else if let Some(joined) = tasks.try_join_next() {
                Some(joined)
            } else {
                break;
            };
            let Some(joined) = joined else {
                break;
            };
            let error = match joined {
                Ok(Ok(())) => None,
                Ok(Err(error)) if error.database_unreachable() => None,
                Ok(Err(error)) => Some(error),
                Err(error) => Some(WorkerError::TaskEnded(error.to_string())),
            };
            let failed = error.is_some();
            if let Some(error) = error {
                first.get_or_insert(error);
            }
            if failed && !wait {
                break;
            }
        }
        if wait
            && let Err(error) = self.fail_open().await
            && first.is_none()
            && !error.database_unreachable()
        {
            first = Some(error);
        }
        match first {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn fail_open(&self) -> Result<(), WorkerError> {
        let pending = {
            let mut open = self.open.lock().await;
            std::mem::take(&mut *open)
        };
        let mut first = None;
        for id in pending {
            if let Err(error) = self.queue.fail(id, "a worker task ended").await
                && first.is_none()
            {
                first = Some(WorkerError::queue(error));
            }
        }
        match first {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

async fn wait_outage(
    notifications: &mut dyn JobNotifications,
    cancel: &CancellationToken,
) -> Result<bool, WorkerError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Ok(false),
        incoming = notifications.recv() => match incoming {
            Ok(()) => Ok(true),
            Err(error) => {
                let wrapped = WorkerError::queue(error);
                if wrapped.database_unreachable() {
                    Ok(crate::outage::wait(crate::outage::FLOOR, cancel).await)
                } else {
                    Err(wrapped)
                }
            }
        },
        () = tokio::time::sleep(crate::outage::FLOOR) => Ok(true),
    }
}
