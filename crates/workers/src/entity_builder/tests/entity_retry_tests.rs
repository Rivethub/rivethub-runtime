use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use chrono::TimeZone;
use chrono::Utc;
use kernel::model::Embedding;
use kernel::model::FinishReason;
use kernel::model::LlmResponse;
use kernel::model::LlmResponseKind;
use kernel::model::ModelCallFailure;
use kernel::model::ModelProvider;
use kernel::model::ModelProviderError;
use kernel::model::ModelRequest;
use kernel::model::ModelStreamItem;
use kernel::queue::ClaimSelector;
use kernel::queue::ClaimedJob;
use kernel::queue::Job;
use kernel::queue::JobId;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::queue::JobQueueError;
use kernel::queue::StoredJob;
use kernel::queue::WorkerRole;
use kernel::wiki::builder::BackfillBudget;
use kernel::wiki::entity_pass::AppliedMark;
use kernel::wiki::entity_pass::CandidateQuery;
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityLease;
use kernel::wiki::entity_pass::EntityPage;
use kernel::wiki::entity_pass::EntityProse;
use kernel::wiki::entity_pass::EntityStore;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::EntityWork;
use kernel::wiki::entity_pass::ExtractEntities;
use kernel::wiki::entity_pass::FactState;
use kernel::wiki::entity_pass::FollowingLeaf;
use kernel::wiki::entity_pass::LiveFact;
use kernel::wiki::entity_pass::MatchCandidate;
use kernel::wiki::entity_pass::MentionNote;
use kernel::wiki::entity_pass::ObserveContext;
use kernel::wiki::entity_pass::PreparedEntity;
use kernel::wiki::entity_pass::PublishOutcome;
use kernel::wiki::entity_pass::StoredRewrite;
use kernel::wiki::entity_pass::TranscriptMessage;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::transient_kind;
use crate::WorkerError;
use crate::entity_builder::EntityBuilderEnv;
use crate::entity_builder::MatcherSettings;
use crate::entity_builder::identify_call;
use crate::entity_builder::rewrite::AcceptedRewrite;
use crate::entity_builder::rewrite::TouchedEntity;
use crate::entity_builder::rewrite::accepted_rewrite;
use crate::entity_builder::rewrite::rewrite_touched;
use crate::outage::OutageCause;

#[derive(Debug)]
pub(super) enum Step {
    Empty,
    MalformedStream,
    Truncated,
    Json(String),
}

#[derive(Debug)]
pub(super) struct Script {
    steps: Mutex<VecDeque<Step>>,
    calls: AtomicUsize,
}

impl Script {
    pub(super) fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: Mutex::new(VecDeque::from(steps)),
            calls: AtomicUsize::new(0),
        }
    }

    pub(super) fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }

    async fn next(&self) -> Option<Step> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let mut steps = self.steps.lock().await;
        steps.pop_front()
    }
}

fn reply(content: Option<String>, finish: FinishReason) -> LlmResponse {
    LlmResponse {
        r#type: LlmResponseKind::Text,
        content,
        tool_calls: None,
        usage: None,
        finish_reason: Some(finish),
    }
}

fn stream_error() -> ModelProviderError {
    ModelProviderError::Stream {
        message: "the model server sent a malformed stream event: missing field `id` at line 1 column 106".to_owned(),
        source: Box::new(std::io::Error::other("malformed stream")),
    }
}

fn idle_provider(message: &str) -> ModelProviderError {
    ModelProviderError::Request {
        message: message.to_owned(),
        source: Box::new(std::io::Error::other(message.to_owned())),
    }
}

#[async_trait]
impl ModelProvider for Script {
    async fn chat(&self, _request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        match self.next().await {
            Some(Step::Empty) => Ok(reply(None, FinishReason::Stop)),
            Some(Step::MalformedStream) => Err(stream_error()),
            Some(Step::Truncated) => Ok(reply(Some("{}".to_owned()), FinishReason::Length)),
            Some(Step::Json(body)) => Ok(reply(Some(body), FinishReason::Stop)),
            None => Err(idle_provider("script has no further chat")),
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        Err(idle_provider("script has no stream"))
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        drop(texts);
        Ok(Vec::new())
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        Ok(Vec::new())
    }
}

#[derive(Debug)]
struct IdleStore;

#[async_trait]
impl EntityStore for IdleStore {
    async fn entity_work(&self, _job: &ExtractEntities) -> Result<EntityWork, EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn following(
        &self,
        _lead: &kernel::wiki::entity_pass::EntityLeaf,
        _limit: u32,
    ) -> Result<Vec<FollowingLeaf>, EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn observe_context(
        &self,
        _leaves: &[kernel::wiki::entity_pass::EntityLeaf],
    ) -> Result<ObserveContext, EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn transcript(
        &self,
        _leaves: &[kernel::wiki::entity_pass::EntityLeaf],
    ) -> Result<Vec<TranscriptMessage>, EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn commit(
        &self,
        _leaves: &[kernel::wiki::entity_pass::EntityLeaf],
        _prepared: &PreparedEntity,
        _now: chrono::DateTime<Utc>,
    ) -> Result<Option<EntityView>, EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn publish(
        &self,
        _view: &EntityView,
        _prose: &EntityProse,
        _now: chrono::DateTime<Utc>,
    ) -> Result<PublishOutcome, EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn finish_entities(
        &self,
        _leaves: &[Uuid],
        _slugs: &[String],
        _git_sha: Option<&str>,
        _now: chrono::DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn fail_entity(
        &self,
        _summary_id: Uuid,
        _error: &str,
        _now: chrono::DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn entity_backfill(
        &self,
        _budget: BackfillBudget,
    ) -> Result<Vec<ExtractEntities>, EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn stale_entities(&self, _limit: u32) -> Result<Vec<ExtractEntities>, EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn revive_dead_entities(&self, _limit: u32) -> Result<u64, EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn match_candidates(
        &self,
        _query: &CandidateQuery,
    ) -> Result<Vec<MatchCandidate>, EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn remember_embedding(
        &self,
        _entity: Uuid,
        _embedding: &[f32],
        _now: chrono::DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn hold_candidate(
        &self,
        _summary: Uuid,
        _prepared: &PreparedEntity,
        _confidence: f32,
        _reason: &str,
        _now: chrono::DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }

    async fn lease_entity(&self, _id: Uuid) -> Result<Box<dyn EntityLease>, EntityStoreError> {
        Err(EntityStoreError::DirectoryMissing)
    }
}

#[derive(Debug)]
struct IdleQueue;

#[async_trait]
impl JobQueue for IdleQueue {
    fn task_names(&self, role: WorkerRole) -> Vec<JobName> {
        role.task_list().to_vec()
    }

    async fn subscribe(&self) -> Result<Box<dyn kernel::queue::JobNotifications>, JobQueueError> {
        Err(JobQueueError::Absent)
    }

    async fn enqueue(&self, _job: Job) -> Result<JobId, JobQueueError> {
        Err(JobQueueError::Absent)
    }

    async fn claim(&self, _selector: &ClaimSelector) -> Result<Option<ClaimedJob>, JobQueueError> {
        Err(JobQueueError::Absent)
    }

    async fn complete(&self, _id: JobId) -> Result<(), JobQueueError> {
        Err(JobQueueError::Absent)
    }

    async fn fail(&self, _id: JobId, _error: &str) -> Result<(), JobQueueError> {
        Err(JobQueueError::Absent)
    }

    async fn release(&self, _id: JobId) -> Result<(), JobQueueError> {
        Err(JobQueueError::Absent)
    }

    async fn flag_stuck(&self) -> Result<u64, JobQueueError> {
        Err(JobQueueError::Absent)
    }

    async fn read(&self, _id: JobId) -> Result<Option<StoredJob>, JobQueueError> {
        Err(JobQueueError::Absent)
    }
}

#[derive(Debug)]
pub(super) struct Harness {
    provider: Script,
    store: IdleStore,
    queue: IdleQueue,
    settings: MatcherSettings,
    cancel: CancellationToken,
    model: String,
    now: chrono::DateTime<Utc>,
}

impl Harness {
    pub(super) fn new(steps: Vec<Step>) -> Self {
        Self {
            provider: Script::new(steps),
            store: IdleStore,
            queue: IdleQueue,
            settings: MatcherSettings::default(),
            cancel: CancellationToken::new(),
            model: "thinkingcap-flash-next".to_owned(),
            now: Utc
                .timestamp_opt(1_700_000_000, 0)
                .single()
                .unwrap_or_default(),
        }
    }

    pub(super) fn env(&self) -> EntityBuilderEnv<'_> {
        EntityBuilderEnv {
            store: &self.store,
            provider: &self.provider,
            queue: &self.queue,
            model: &self.model,
            settings: &self.settings,
            now: self.now,
            cancel: &self.cancel,
        }
    }

    pub(super) fn calls(&self) -> usize {
        self.provider.calls()
    }
}

fn identified(name: &str) -> String {
    format!(r#"{{"observations":[{{"entity_ref":null,"name":"{name}","type":"Hosts"}}]}}"#)
}

async fn identify(harness: &Harness) -> Result<Vec<String>, WorkerError> {
    let observed = identify_call(&harness.env(), "preface", "transcript", &[], &[]).await?;
    Ok(observed.into_iter().map(|item| item.name).collect())
}

#[tokio::test(start_paused = true)]
async fn an_empty_reply_then_a_good_one_succeeds() {
    let harness = Harness::new(vec![Step::Empty, Step::Json(identified("gpubox"))]);
    let names = identify(&harness).await;
    assert!(
        names
            .as_ref()
            .is_ok_and(|found| found == &["gpubox".to_owned()]),
        "{}",
        names
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default()
    );
    assert_eq!(harness.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_stream_error_then_a_good_reply_succeeds() {
    let harness = Harness::new(vec![Step::MalformedStream, Step::Json(identified("gerty"))]);
    let names = identify(&harness).await;
    assert!(
        names
            .as_ref()
            .is_ok_and(|found| found == &["gerty".to_owned()]),
        "{}",
        names
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default()
    );
    assert_eq!(harness.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn two_failures_still_fail_the_call() {
    let harness = Harness::new(vec![Step::Empty, Step::Empty]);
    let names = identify(&harness).await;
    let message = names
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default();
    assert!(message.contains("empty summary"), "{message}");
    assert_eq!(harness.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn two_stream_errors_get_a_last_chance_that_succeeds() {
    let harness = Harness::new(vec![
        Step::MalformedStream,
        Step::MalformedStream,
        Step::Json(identified("datahub")),
    ]);
    let names = identify(&harness).await;
    assert!(
        names
            .as_ref()
            .is_ok_and(|found| found == &["datahub".to_owned()]),
        "{}",
        names
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default()
    );
    assert_eq!(harness.calls(), 3);
}

#[tokio::test(start_paused = true)]
async fn three_stream_errors_fail_the_call() {
    let harness = Harness::new(vec![
        Step::MalformedStream,
        Step::MalformedStream,
        Step::MalformedStream,
    ]);
    let names = identify(&harness).await;
    assert!(names.is_err());
    assert_eq!(harness.calls(), 3);
}

fn stamp() -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000, 0)
        .single()
        .unwrap_or_default()
}

fn host_view() -> EntityView {
    EntityView {
        id: Uuid::from_u128(3),
        name: "gpubox".to_owned(),
        slug: "gpubox".to_owned(),
        type_name: "Hosts".to_owned(),
        type_slug: "hosts".to_owned(),
        aliases: Vec::new(),
        facts: Vec::new(),
        history: Vec::new(),
        snapshot: String::new(),
        owner: Some("owner".to_owned()),
    }
}

fn host_leaf() -> EntityLeaf {
    EntityLeaf {
        id: Uuid::from_u128(4),
        conversation_id: Some(Uuid::from_u128(9)),
        content: "leaf".to_owned(),
        start: stamp(),
        end: stamp(),
        owner: Some("owner".to_owned()),
    }
}

fn page_with_ram() -> EntityPage {
    EntityPage {
        summary: "Host.".to_owned(),
        article: "Lead.".to_owned(),
        facts: vec![LiveFact {
            attribute: "ram".to_owned(),
            value: "87 GB available".to_owned(),
            as_of: stamp(),
            status: FactState::Current,
        }],
        observed_types: Vec::new(),
        fixed: Vec::new(),
    }
}

#[tokio::test(start_paused = true)]
async fn a_rewrite_that_drops_every_fact_is_retried_once() {
    let dropped = r#"{"summary":"Host.","article":"Lead.","current_facts":[],"retired":[{"attribute":"ram","kind":"contradicted","detail":"ram was removed"}],"history_sentence":"Nothing."}"#;
    let kept = r#"{"summary":"Host with 152 GB.","article":"Lead.","current_facts":[{"attribute":"ram","value":"152 GB available","as_of":"2023-11-14T22:13:20Z"}],"history_sentence":"RAM is 152 GB."}"#;
    let harness = Harness::new(vec![
        Step::Json(dropped.to_owned()),
        Step::Json(kept.to_owned()),
    ]);
    let rewritten = accepted_rewrite(
        &harness.env(),
        &host_view(),
        &[host_leaf()],
        &page_with_ram(),
        "ram was removed",
        false,
        "ram was removed",
    )
    .await;
    assert!(
        rewritten.as_ref().is_ok_and(|body| {
            matches!(
                body,
                AcceptedRewrite::Page(page)
                    if page.summary == "Host with 152 GB." && page.facts.len() == 1
            )
        }),
        "{}",
        rewritten
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default()
    );
    assert_eq!(harness.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_truncated_identify_is_not_retried() {
    let harness = Harness::new(vec![Step::Truncated, Step::Json(identified("gpubox"))]);
    let names = identify(&harness).await;
    assert!(matches!(names, Err(WorkerError::Truncated)));
    assert_eq!(harness.calls(), 1);
}

#[test]
fn transient_kinds_name_the_measured_failures() {
    assert_eq!(transient_kind(&WorkerError::EmptySummary), Some("empty"));
    assert_eq!(transient_kind(&WorkerError::Truncated), None);
    let stream = WorkerError::WikiModel(stream_error());
    assert_eq!(transient_kind(&stream), Some("malformed-stream"));
    let ended = WorkerError::WikiModel(ModelProviderError::Stream {
        message: "the model stream ended before Done".to_owned(),
        source: Box::new(std::io::Error::other("ended")),
    });
    assert_eq!(transient_kind(&ended), Some("stream-ended"));
    let reset = WorkerError::WikiModel(ModelProviderError::Request {
        message: "the model server request failed: connection reset by peer".to_owned(),
        source: Box::new(std::io::Error::other("connection reset by peer")),
    });
    assert_eq!(transient_kind(&reset), Some("connection-reset"));
    let timed_out = WorkerError::WikiModel(ModelProviderError::Request {
        message: "the model server request failed: operation timed out".to_owned(),
        source: Box::new(std::io::Error::other("operation timed out")),
    });
    assert_eq!(transient_kind(&timed_out), Some("timeout"));
    assert_eq!(
        transient_kind(&WorkerError::ModelTimeout { seconds: 45 }),
        Some("timeout")
    );
    assert_eq!(
        transient_kind(&WorkerError::ProviderOutage {
            cause: OutageCause::ServerError,
            retry_after: None,
        }),
        Some("http-5xx")
    );
    let http = WorkerError::WikiModel(ModelProviderError::Request {
        message: "model server returned 503".to_owned(),
        source: Box::new(ModelCallFailure::Http {
            status: 503,
            retry_after: None,
            source: Box::new(std::io::Error::other("503")),
        }),
    });
    assert_eq!(transient_kind(&http), Some("http-5xx"));
    assert_eq!(
        transient_kind(&WorkerError::Llm {
            message: "rewrite of gpubox dropped every current fact".to_owned(),
        }),
        Some("dropped-facts")
    );
    assert_eq!(
        transient_kind(&WorkerError::ProviderOutage {
            cause: OutageCause::Credentials,
            retry_after: None,
        }),
        None
    );
}

#[tokio::test]
async fn a_database_error_retries_the_session_without_rewriting_the_applied_entity() {
    let kept = Uuid::from_u128(11);
    let lost = Uuid::from_u128(12);
    let store = RetryStore::new(lost);
    let provider = NamedModel::default();
    let queue = IdleQueue;
    let mut settings = MatcherSettings::default();
    settings.embed_url.clear();
    let cancel = CancellationToken::new();
    let model = "thinkingcap-flash-next";
    let env = EntityBuilderEnv {
        store: &store,
        provider: &provider,
        queue: &queue,
        model,
        settings: &settings,
        now: stamp(),
        cancel: &cancel,
    };
    let leaf = host_leaf();
    let messages = vec![said_line("keptbox is up"), said_line("lostbox is up")];
    let touched = vec![named_entity(kept, "keptbox"), named_entity(lost, "lostbox")];
    let first = rewrite_touched(&env, std::slice::from_ref(&leaf), &messages, &touched).await;
    assert!(matches!(
        first,
        Err(WorkerError::EntityStore(EntityStoreError::Query(_)))
    ));
    let second = rewrite_touched(&env, std::slice::from_ref(&leaf), &messages, &touched)
        .await
        .expect("retry");
    assert_eq!(second.len(), 2);
    assert!(
        second
            .iter()
            .any(|page| page.slug == "keptbox" && page.git_sha.is_empty())
    );
    assert!(second.iter().any(|page| !page.git_sha.is_empty()));
    assert_eq!(store.applies(kept).await, 1);
    assert_eq!(store.applies(lost).await, 1);
    assert_eq!(provider.mentions("keptbox").await, 1);
    assert_eq!(provider.mentions("lostbox").await, 2);

    let cut = rewrite_touched(
        &env,
        std::slice::from_ref(&leaf),
        &[said_line("cutshort is up")],
        &[named_entity(Uuid::from_u128(13), "cutshort")],
    )
    .await
    .expect("truncated");
    assert!(cut.is_empty());

    let garbled = rewrite_touched(
        &env,
        std::slice::from_ref(&leaf),
        &[said_line("garbledhost is up")],
        &[named_entity(Uuid::from_u128(14), "garbledhost")],
    )
    .await;
    assert!(matches!(garbled, Err(WorkerError::WikiJson(_))));

    let quiet = rewrite_touched(
        &env,
        &[leaf],
        &[],
        &[named_entity(Uuid::from_u128(15), "quietbox")],
    )
    .await
    .expect("no evidence");
    assert!(quiet.is_empty());
    assert_eq!(store.applies(Uuid::from_u128(15)).await, 0);
}

fn said_line(content: &str) -> TranscriptMessage {
    TranscriptMessage {
        id: Uuid::new_v4(),
        at: stamp(),
        role: "user".to_owned(),
        tool_name: None,
        content: content.to_owned(),
        tool_args: None,
        tool_result: None,
    }
}

fn named_entity(id: Uuid, name: &str) -> TouchedEntity {
    TouchedEntity {
        view: EntityView {
            id,
            name: name.to_owned(),
            slug: name.to_owned(),
            type_name: "Hosts".to_owned(),
            type_slug: "hosts".to_owned(),
            aliases: Vec::new(),
            facts: Vec::new(),
            history: Vec::new(),
            snapshot: String::new(),
            owner: Some("owner".to_owned()),
        },
        mention: name.to_owned(),
    }
}

#[derive(Default)]
struct NamedModel {
    calls: Mutex<Vec<String>>,
}

impl NamedModel {
    async fn mentions(&self, name: &str) -> usize {
        let calls = self.calls.lock().await;
        calls.iter().filter(|call| call.contains(name)).count()
    }
}

#[async_trait]
impl ModelProvider for NamedModel {
    async fn chat(&self, request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        let rendered = format!("{:?}", request.messages);
        self.calls.lock().await.push(rendered.clone());
        if rendered.contains("cutshort") {
            return Ok(reply(Some("{}".to_owned()), FinishReason::Length));
        }
        if rendered.contains("garbledhost") {
            return Ok(reply(Some("not-json".to_owned()), FinishReason::Stop));
        }
        Ok(reply(
            Some(
                r#"{"summary":"Host.","article":"Lead.","current_facts":[],"history_sentence":"Seen."}"#
                    .to_owned(),
            ),
            FinishReason::Stop,
        ))
    }

    async fn stream(
        &self,
        _request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        Err(idle_provider("retry model does not stream"))
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        drop(texts);
        Ok(Vec::new())
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        Ok(Vec::new())
    }
}

struct Ledger {
    applies: std::collections::HashMap<Uuid, usize>,
    marks: std::collections::HashMap<Uuid, AppliedMark>,
    fail_once: Uuid,
    failed: bool,
}

struct RetryStore {
    ledger: std::sync::Arc<Mutex<Ledger>>,
}

impl RetryStore {
    fn new(fail_once: Uuid) -> Self {
        Self {
            ledger: std::sync::Arc::new(Mutex::new(Ledger {
                applies: std::collections::HashMap::new(),
                marks: std::collections::HashMap::new(),
                fail_once,
                failed: false,
            })),
        }
    }

    async fn applies(&self, id: Uuid) -> usize {
        self.ledger
            .lock()
            .await
            .applies
            .get(&id)
            .copied()
            .unwrap_or(0)
    }
}

fn missing() -> EntityStoreError {
    EntityStoreError::DirectoryMissing
}

#[async_trait]
impl EntityStore for RetryStore {
    async fn entity_work(&self, _job: &ExtractEntities) -> Result<EntityWork, EntityStoreError> {
        Err(missing())
    }

    async fn following(
        &self,
        _lead: &EntityLeaf,
        _limit: u32,
    ) -> Result<Vec<FollowingLeaf>, EntityStoreError> {
        Err(missing())
    }

    async fn observe_context(
        &self,
        _leaves: &[EntityLeaf],
    ) -> Result<ObserveContext, EntityStoreError> {
        Err(missing())
    }

    async fn transcript(
        &self,
        _leaves: &[EntityLeaf],
    ) -> Result<Vec<TranscriptMessage>, EntityStoreError> {
        Err(missing())
    }

    async fn commit(
        &self,
        _leaves: &[EntityLeaf],
        _prepared: &PreparedEntity,
        _now: chrono::DateTime<Utc>,
    ) -> Result<Option<EntityView>, EntityStoreError> {
        Err(missing())
    }

    async fn publish(
        &self,
        _view: &EntityView,
        _prose: &EntityProse,
        _now: chrono::DateTime<Utc>,
    ) -> Result<PublishOutcome, EntityStoreError> {
        Err(missing())
    }

    async fn finish_entities(
        &self,
        _leaves: &[Uuid],
        _slugs: &[String],
        _git_sha: Option<&str>,
        _now: chrono::DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        Err(missing())
    }

    async fn fail_entity(
        &self,
        _summary_id: Uuid,
        _error: &str,
        _now: chrono::DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        Err(missing())
    }

    async fn entity_backfill(
        &self,
        _budget: BackfillBudget,
    ) -> Result<Vec<ExtractEntities>, EntityStoreError> {
        Err(missing())
    }

    async fn stale_entities(&self, _limit: u32) -> Result<Vec<ExtractEntities>, EntityStoreError> {
        Err(missing())
    }

    async fn revive_dead_entities(&self, _limit: u32) -> Result<u64, EntityStoreError> {
        Err(missing())
    }

    async fn match_candidates(
        &self,
        _query: &CandidateQuery,
    ) -> Result<Vec<MatchCandidate>, EntityStoreError> {
        Err(missing())
    }

    async fn remember_embedding(
        &self,
        _entity: Uuid,
        _embedding: &[f32],
        _now: chrono::DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        Err(missing())
    }

    async fn hold_candidate(
        &self,
        _summary: Uuid,
        _prepared: &PreparedEntity,
        _confidence: f32,
        _reason: &str,
        _now: chrono::DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        Err(missing())
    }

    async fn lease_entity(&self, id: Uuid) -> Result<Box<dyn EntityLease>, EntityStoreError> {
        Ok(Box::new(MarkLease {
            id,
            ledger: std::sync::Arc::clone(&self.ledger),
        }))
    }
}

struct MarkLease {
    id: Uuid,
    ledger: std::sync::Arc<Mutex<Ledger>>,
}

#[async_trait]
impl EntityLease for MarkLease {
    async fn page(&mut self) -> Result<EntityPage, EntityStoreError> {
        Ok(EntityPage {
            summary: String::new(),
            article: String::new(),
            facts: Vec::new(),
            observed_types: Vec::new(),
            fixed: Vec::new(),
        })
    }

    async fn applied_through(&mut self) -> Result<Option<AppliedMark>, EntityStoreError> {
        Ok(self.ledger.lock().await.marks.get(&self.id).copied())
    }

    async fn apply(
        &mut self,
        body: &StoredRewrite,
        _now: chrono::DateTime<Utc>,
    ) -> Result<PublishOutcome, EntityStoreError> {
        let mut ledger = self.ledger.lock().await;
        if self.id == ledger.fail_once && !ledger.failed {
            ledger.failed = true;
            drop(ledger);
            return Err(EntityStoreError::Query(Box::new(std::io::Error::other(
                "database",
            ))));
        }
        *ledger.applies.entry(self.id).or_insert(0) += 1;
        ledger.marks.insert(
            self.id,
            AppliedMark {
                at: body.session_at,
                summary_id: body.session_id,
            },
        );
        drop(ledger);
        Ok(PublishOutcome::Stored {
            git_sha: "abc".to_owned(),
            slug: "page".to_owned(),
        })
    }

    async fn record_mention(
        &mut self,
        _note: &MentionNote,
        _now: chrono::DateTime<Utc>,
    ) -> Result<Option<String>, EntityStoreError> {
        Ok(None)
    }

    async fn release(&mut self) -> Result<(), EntityStoreError> {
        Ok(())
    }
}
