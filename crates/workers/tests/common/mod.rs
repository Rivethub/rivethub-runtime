use std::sync::atomic::AtomicI64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use kernel::model::Embedding;
use kernel::model::LlmResponse;
use kernel::model::ModelProvider;
use kernel::model::ModelProviderError;
use kernel::model::ModelRequest;
use kernel::model::ModelStreamItem;
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
use serde::Serialize;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Receiver;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;
mod store;

pub use store::Store;

use workers::EmbedSettings;
use workers::WorkerError;
use workers::handle_embed;

pub const WIDTH: u32 = 8;

pub fn settings() -> EmbedSettings {
    EmbedSettings::standard().with_width(WIDTH)
}

pub fn width() -> usize {
    usize::try_from(WIDTH).expect("the test width fits usize")
}

pub fn target_payload(table: &str, id: &str) -> JsonObject {
    JsonObject::from_serialize(&TargetBody {
        target_table: table.to_owned(),
        target_id: id.to_owned(),
    })
    .expect("an embed target payload serializes")
}

pub async fn embed_target(
    store: &Store,
    provider: &dyn ModelProvider,
    embed_settings: EmbedSettings,
    payload: &JsonObject,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    handle_embed(store, provider, embed_settings, payload, cancel).await
}

pub async fn wait_bool(rx: &mut watch::Receiver<bool>, want: bool) {
    wait_watch(rx, |value| *value == want, "boolean watch").await;
}

pub async fn wait_at_least(rx: &mut watch::Receiver<usize>, count: usize) {
    wait_watch(rx, |value| *value >= count, "count watch").await;
}

async fn wait_watch<T>(rx: &mut watch::Receiver<T>, ready: impl Fn(&T) -> bool, label: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if ready(&rx.borrow()) {
                return;
            }
            rx.changed().await.expect(label);
        }
    })
    .await
    .expect(label);
}

pub fn same_bits(actual: &[f32], expected: &[f32]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(left, right)| left.to_bits() == right.to_bits())
}

#[derive(Serialize)]
struct TargetBody {
    #[serde(rename = "targetTable")]
    target_table: String,
    #[serde(rename = "targetId")]
    target_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Ready,
    Running,
    Backoff,
    Dead,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobView {
    pub id: JobId,
    pub name: String,
    pub phase: Phase,
    pub attempt: u32,
    pub error: Option<String>,
}

#[derive(Debug)]
struct Queued {
    id: JobId,
    name: JobName,
    payload: JsonObject,
    attempt: u32,
    max_attempts: u32,
    phase: Phase,
    bypass: bool,
    priority: i16,
    last_error: Option<String>,
}

#[derive(Debug)]
pub struct Queue {
    rows: Mutex<Vec<Queued>>,
    next_id: AtomicI64,
    claim_error: Mutex<Option<JobQueueError>>,
    claims: AtomicUsize,
    claim_gate: watch::Sender<bool>,
    held: AtomicUsize,
    events: watch::Sender<u64>,
    notifications: watch::Sender<()>,
}

impl Queue {
    pub fn new() -> Self {
        let (events, _) = watch::channel(0);
        Self {
            rows: Mutex::new(Vec::new()),
            next_id: AtomicI64::new(1),
            claim_error: Mutex::new(None),
            claims: AtomicUsize::new(0),
            claim_gate: watch::channel(true).0,
            held: AtomicUsize::new(0),
            events,
            notifications: watch::channel(()).0,
        }
    }

    pub async fn push(&self, name: JobName, payload: JsonObject, max_attempts: u32) -> JobId {
        let id = self
            .push_without_notification(name, payload, max_attempts)
            .await;
        self.notifications.send_replace(());
        id
    }

    pub async fn push_without_notification(
        &self,
        name: JobName,
        payload: JsonObject,
        max_attempts: u32,
    ) -> JobId {
        self.add(name, payload, 0, max_attempts, false).await
    }

    pub async fn push_any(&self, name: JobName, payload: JsonObject, max_attempts: u32) -> JobId {
        self.add(name, payload, 0, max_attempts, true).await
    }

    pub async fn fail_next_claim(&self) {
        let error = JobQueueError::Unavailable(Box::new(std::io::Error::other("queue down")));
        *self.claim_error.lock().await = Some(error);
    }

    pub fn hold_claims(&self) {
        self.claim_gate.send_replace(false);
    }

    pub fn open_claims(&self) {
        self.claim_gate.send_replace(true);
    }

    pub async fn wait_held(&self, count: usize) {
        let mut events = self.events.subscribe();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self.held.load(Ordering::Relaxed) >= count {
                    return;
                }
                events.changed().await.expect("queue watch");
            }
        })
        .await
        .expect("the held claim count");
    }

    async fn wait_open(&self) {
        let mut gate = self.claim_gate.subscribe();
        if *gate.borrow_and_update() {
            return;
        }
        self.held.fetch_add(1, Ordering::Relaxed);
        self.bump();
        while !*gate.borrow_and_update() {
            gate.changed().await.expect("claim gate");
        }
    }

    pub fn claims(&self) -> usize {
        self.claims.load(Ordering::Relaxed)
    }

    pub async fn wait_claims(&self, count: usize) {
        let mut events = self.events.subscribe();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self.claims() >= count {
                    return;
                }
                events.changed().await.expect("queue watch");
            }
        })
        .await
        .expect("the claim count");
    }

    pub async fn view(&self, id: JobId) -> JobView {
        self.rows
            .lock()
            .await
            .iter()
            .find(|row| row.id == id)
            .map(snapshot)
            .expect("the queued job exists")
    }

    pub async fn wait_phase(&self, id: JobId, phase: Phase) {
        let mut events = self.events.subscribe();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self.phase_of(id).await == Some(phase) {
                    return;
                }
                events.changed().await.expect("queue watch");
            }
        })
        .await
        .expect("the job phase");
    }

    async fn phase_of(&self, id: JobId) -> Option<Phase> {
        self.rows
            .lock()
            .await
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.phase)
    }

    async fn add(
        &self,
        name: JobName,
        payload: JsonObject,
        priority: i16,
        max_attempts: u32,
        bypass: bool,
    ) -> JobId {
        let id = JobId::new(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.rows.lock().await.push(Queued {
            id,
            name,
            payload,
            attempt: 0,
            max_attempts,
            phase: Phase::Ready,
            bypass,
            priority,
            last_error: None,
        });
        self.bump();
        id
    }

    fn bump(&self) {
        self.events.send_modify(|tick| {
            *tick = tick.wrapping_add(1);
        });
    }
}

fn snapshot(row: &Queued) -> JobView {
    JobView {
        id: row.id,
        name: row.name.to_string(),
        phase: row.phase,
        attempt: row.attempt,
        error: row.last_error.clone(),
    }
}

struct Landing<'a> {
    queue: &'a Queue,
    selector: &'a ClaimSelector,
    armed: bool,
}

impl Drop for Landing<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(mut rows) = self.queue.rows.try_lock() else {
            return;
        };
        let landed = claim_row(&mut rows, self.selector);
        drop(rows);
        if matches!(landed, Ok(Some(_))) {
            self.queue.claims.fetch_add(1, Ordering::Relaxed);
        }
        self.queue.bump();
    }
}

fn claim_row(
    rows: &mut [Queued],
    selector: &ClaimSelector,
) -> Result<Option<ClaimedJob>, JobQueueError> {
    let Some(index) = select_ready(rows, selector) else {
        return Ok(None);
    };
    let Some(row) = rows.get_mut(index) else {
        return Err(JobQueueError::Claim {
            message: "the ready job index is outside the queue".to_owned(),
            source: Box::new(std::io::Error::other("ready job index")),
        });
    };
    row.attempt = row.attempt.saturating_add(1);
    row.phase = Phase::Running;
    Ok(Some(ClaimedJob {
        id: row.id,
        name: row.name.clone(),
        payload: row.payload.clone(),
        attempt: row.attempt,
    }))
}

fn select_ready(rows: &[Queued], selector: &ClaimSelector) -> Option<usize> {
    let mut chosen: Option<usize> = None;
    for (index, row) in rows.iter().enumerate() {
        let named = selector.names.iter().any(|name| name == &row.name);
        let pinned = selector.only_id.is_none_or(|only| only == row.id);
        if row.phase != Phase::Ready || !(row.bypass || (named && pinned)) {
            continue;
        }
        chosen = Some(match chosen {
            None => index,
            Some(current) => prefer(rows, current, index),
        });
    }
    chosen
}

fn prefer(rows: &[Queued], current: usize, index: usize) -> usize {
    let Some(left) = rows.get(current) else {
        return index;
    };
    let Some(right) = rows.get(index) else {
        return current;
    };
    if right.priority < left.priority || (right.priority == left.priority && right.id < left.id) {
        index
    } else {
        current
    }
}

fn default_max_attempts(name: &JobName) -> u32 {
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

fn missing(id: JobId) -> JobQueueError {
    JobQueueError::Claim {
        message: format!("job {id} is not running"),
        source: Box::new(std::io::Error::other("job is not running")),
    }
}

#[async_trait]
impl JobQueue for Queue {
    fn task_names(&self, role: WorkerRole) -> Vec<JobName> {
        workers::jobs_for(role)
    }

    async fn subscribe(&self) -> Result<Box<dyn kernel::queue::JobNotifications>, JobQueueError> {
        Ok(Box::new(self.notifications.subscribe()))
    }

    async fn enqueue(&self, job: Job) -> Result<JobId, JobQueueError> {
        let max_attempts = match job.max_attempts {
            Some(attempts) => u32::try_from(attempts)
                .map_err(|_| JobQueueError::Stored("max_attempts".to_owned()))?,
            None => default_max_attempts(&job.name),
        };
        let id = self
            .add(job.name, job.payload, job.priority, max_attempts, false)
            .await;
        self.notifications.send_replace(());
        Ok(id)
    }

    async fn claim(&self, selector: &ClaimSelector) -> Result<Option<ClaimedJob>, JobQueueError> {
        let pending = self.claim_error.lock().await.take();
        if let Some(error) = pending {
            self.claims.fetch_add(1, Ordering::Relaxed);
            self.bump();
            return Err(error);
        }
        let mut landing = Landing {
            queue: self,
            selector,
            armed: true,
        };
        self.wait_open().await;
        landing.armed = false;
        let claimed = claim_row(&mut self.rows.lock().await, selector)?;
        if claimed.is_none() {
            return Ok(None);
        }
        self.claims.fetch_add(1, Ordering::Relaxed);
        self.bump();
        Ok(claimed)
    }

    async fn complete(&self, id: JobId) -> Result<(), JobQueueError> {
        {
            let mut rows = self.rows.lock().await;
            let Some(row) = rows.iter_mut().find(|row| row.id == id) else {
                return Err(missing(id));
            };
            if row.phase != Phase::Running {
                return Err(missing(id));
            }
            row.phase = Phase::Done;
            drop(rows);
        }
        self.bump();
        Ok(())
    }

    async fn release(&self, id: JobId) -> Result<(), JobQueueError> {
        {
            let mut rows = self.rows.lock().await;
            let Some(row) = rows.iter_mut().find(|row| row.id == id) else {
                return Err(missing(id));
            };
            if row.phase != Phase::Running {
                return Err(missing(id));
            }
            row.phase = Phase::Ready;
            row.attempt = row.attempt.saturating_sub(1);
            drop(rows);
        }
        self.bump();
        Ok(())
    }

    async fn fail(&self, id: JobId, error: &str) -> Result<(), JobQueueError> {
        {
            let mut rows = self.rows.lock().await;
            let Some(row) = rows.iter_mut().find(|row| row.id == id) else {
                return Err(missing(id));
            };
            if row.phase != Phase::Running {
                return Err(missing(id));
            }
            row.last_error = Some(error.to_owned());
            if row.attempt >= row.max_attempts {
                row.phase = Phase::Dead;
            } else {
                row.phase = Phase::Backoff;
            }
            drop(rows);
        }
        self.bump();
        Ok(())
    }

    async fn flag_stuck(&self) -> Result<u64, JobQueueError> {
        let mut rows = self.rows.lock().await;
        let before = rows.len();
        rows.retain(|row| row.phase != Phase::Dead);
        let flagged = before - rows.len();
        drop(rows);
        u64::try_from(flagged).map_err(|_| JobQueueError::Stored("flagged".to_owned()))
    }

    async fn read(&self, id: JobId) -> Result<Option<StoredJob>, JobQueueError> {
        let rows = self.rows.lock().await;
        let stored = rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| {
                Ok(StoredJob {
                    id: row.id,
                    name: row.name.clone(),
                    payload: row.payload.clone(),
                    key: None,
                    priority: Some(row.priority),
                    max_attempts: i32::try_from(row.max_attempts)
                        .map_err(|_| JobQueueError::Stored("max_attempts".to_owned()))?,
                    attempts: row.attempt,
                    last_error: row.last_error.clone(),
                })
            })
            .transpose();
        drop(rows);
        stored
    }
}

#[derive(Debug)]
pub struct HangEmbed {
    started: watch::Sender<bool>,
}

impl HangEmbed {
    pub fn new() -> Self {
        let (started, _) = watch::channel(false);
        Self { started }
    }

    pub fn started(&self) -> watch::Receiver<bool> {
        self.started.subscribe()
    }
}

fn rejected(message: &str) -> ModelProviderError {
    ModelProviderError::Request {
        message: message.to_owned(),
        source: Box::new(std::io::Error::other(message.to_owned())),
    }
}

#[async_trait]
impl ModelProvider for HangEmbed {
    async fn chat(&self, _request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        Err(rejected("the hang provider has no chat"))
    }

    async fn stream(
        &self,
        _request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        Err(rejected("the hang provider has no stream"))
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        drop(texts);
        let _previous = self.started.send_replace(true);
        std::future::pending().await
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        Ok(Vec::new())
    }
}
