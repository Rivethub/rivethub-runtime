use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use kernel::owner::Owner;
use kernel::tenancy::Scope;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::attempt::Ctx;
use crate::attempt::LeadEnd;
use crate::attempt::Leaf;
use crate::attempt::run_lead;
use crate::failure::Failure;
use crate::failure::database;
use crate::failure::note;
use crate::failure::redact;
use crate::lineage::EmptyLineage;
use crate::logged::LoggedProvider;
use crate::logged::Totals;

const COALESCE_MORE: usize = 7;

#[derive(Debug)]
pub(crate) struct JobOutcome {
    pub(crate) summary_id: Uuid,
    pub(crate) outcome: &'static str,
    pub(crate) error: Option<String>,
}

pub(crate) struct Engine {
    ctx: Arc<Ctx>,
    workers: Workers,
}

impl Engine {
    pub(crate) fn totals(&self) -> Totals {
        self.ctx.provider.totals()
    }

    pub(crate) fn pool(&self) -> &PgPool {
        self.ctx.memory.pool()
    }

    pub(crate) async fn log_error(&self) -> Option<String> {
        self.ctx.provider.log_error().await
    }
}

pub(crate) struct StepDone {
    pub(crate) jobs: Vec<JobOutcome>,
    pub(crate) failure: Option<Failure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Workers {
    Entities,
    Topics,
    Both,
}

impl Workers {
    pub(crate) fn parse(text: &str) -> Result<Self, Failure> {
        match text {
            "entities" => Ok(Self::Entities),
            "topics" => Ok(Self::Topics),
            "both" => Ok(Self::Both),
            other => Err(Failure::Usage {
                detail: format!("--worker must be entities, topics, or both, not {other}"),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pass {
    Entities,
    Topics,
}

pub(crate) struct Run<'a> {
    pub(crate) app: &'a str,
    pub(crate) pages: &'a Path,
    pub(crate) llm_dir: &'a Path,
    pub(crate) principal: &'a str,
    pub(crate) model: &'a str,
    pub(crate) base_url: &'a str,
    pub(crate) api_key_file: Option<&'a Path>,
    pub(crate) no_reasoning: bool,
    pub(crate) secrets: &'a [String],
    pub(crate) worker: Workers,
    pub(crate) matcher: &'a workers::entity_builder::MatcherSettings,
}

pub(crate) async fn open(run: &Run<'_>) -> Result<Engine, Failure> {
    let provider = LoggedProvider::new(
        build_provider(run)?,
        run.llm_dir.to_path_buf(),
        run.secrets.to_vec(),
    );
    let owner =
        Owner::new(run.principal.to_owned()).map_err(|error| Failure::Owner(error.to_string()))?;
    let memory = PostgresMemory::connect(run.app)
        .await
        .map_err(|error| database(&error, run.secrets))?
        .with_wiki_directory(run.pages.to_path_buf())
        .with_wiki_owner(owner);
    let memory = Arc::new(memory);
    let queue = Arc::new(
        memory
            .queue()
            .await
            .map_err(|error| Failure::Queue(redact(&error.to_string(), run.secrets)))?,
    );
    Ok(Engine {
        ctx: Arc::new(Ctx {
            memory,
            queue,
            provider: Arc::new(provider),
            lineage: EmptyLineage,
            model: run.model.to_owned(),
            cancel: CancellationToken::new(),
            secrets: run.secrets.to_vec(),
            matcher: run.matcher.clone(),
        }),
        workers: run.worker,
    })
}

pub(crate) async fn held_count(engine: &Engine) -> Result<i64, Failure> {
    sqlx::query_scalar("SELECT count(*) FROM rh_entity_candidates")
        .fetch_one(engine.pool())
        .await
        .map_err(|error| database(&error, &engine.ctx.secrets))
}

pub(crate) async fn order(engine: &Engine, ids: &[Uuid]) -> Result<Vec<Uuid>, Failure> {
    let rows = sqlx::query_as::<_, Started>(ORDER)
        .bind(ids)
        .fetch_all(engine.pool())
        .await
        .map_err(|error| database(&error, &engine.ctx.secrets))?;
    Ok(sort_started(ids, rows))
}

pub(crate) async fn conversation(engine: &Engine, id: Uuid, concurrency: usize) -> StepDone {
    let done = match engine.workers {
        Workers::Entities => one_pass(engine, id, concurrency, Pass::Entities).await,
        Workers::Topics => one_pass(engine, id, concurrency, Pass::Topics).await,
        Workers::Both => {
            let entities = one_pass(engine, id, concurrency, Pass::Entities).await;
            if entities.failure.is_some() {
                let retired = retire(engine.pool(), &engine.ctx.secrets).await.err();
                return StepDone {
                    jobs: entities.jobs,
                    failure: entities.failure.or(retired),
                };
            }
            one_pass(engine, id, concurrency, Pass::Topics).await
        }
    };
    let retired = retire(engine.pool(), &engine.ctx.secrets).await.err();
    StepDone {
        jobs: done.jobs,
        failure: done.failure.or(retired),
    }
}

async fn one_pass(engine: &Engine, id: Uuid, concurrency: usize, pass: Pass) -> StepDone {
    let board = match leaves(engine.pool(), id, &engine.ctx.secrets).await {
        Ok(board) => board,
        Err(error) => {
            return StepDone {
                jobs: Vec::new(),
                failure: Some(error),
            };
        }
    };
    if board.leaves.is_empty() {
        note(&format!("{id} has no leaf summaries"));
        return StepDone {
            jobs: Vec::new(),
            failure: None,
        };
    }
    note(&format!("{} leaf summaries in {id}", board.leaves.len()));
    let gate = Arc::new(Mutex::new(Gate {
        board,
        busy: HashSet::new(),
        pass,
    }));
    let drained = drain(Arc::clone(&gate), &engine.ctx, concurrency).await;
    let jobs = {
        let guard = gate.lock().await;
        outcomes(&guard.board)
    };
    StepDone {
        jobs,
        failure: drained,
    }
}

#[derive(Debug, sqlx::FromRow)]
struct Started {
    id: Uuid,
    started: Option<chrono::DateTime<chrono::Utc>>,
}

fn sort_started(ids: &[Uuid], mut rows: Vec<Started>) -> Vec<Uuid> {
    rows.sort_by(|left, right| {
        stamp(left.started)
            .cmp(&stamp(right.started))
            .then_with(|| index_of(ids, left.id).cmp(&index_of(ids, right.id)))
    });
    let mut ordered: Vec<Uuid> = rows.into_iter().map(|row| row.id).collect();
    for id in ids {
        if !ordered.contains(id) {
            ordered.push(*id);
        }
    }
    ordered
}

fn stamp(started: Option<chrono::DateTime<chrono::Utc>>) -> (u8, i64) {
    match started {
        Some(time) => (0, time.timestamp()),
        None => (1, 0),
    }
}

fn index_of(ids: &[Uuid], id: Uuid) -> usize {
    ids.iter()
        .position(|item| *item == id)
        .unwrap_or(usize::MAX)
}

async fn retire(pool: &PgPool, secrets: &[String]) -> Result<(), Failure> {
    sqlx::query(RETIRE)
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|error| database(&error, secrets))
}

fn build_provider(run: &Run<'_>) -> Result<providers::OpenAiCompatible, Failure> {
    let settings = config::provider::OpenaiProvider {
        model: Some(run.model.to_owned()),
        base_url: Some(run.base_url.to_owned()),
        probe_models: Some(false),
        api_key_file: run.api_key_file.map(Path::to_path_buf),
        reasoning: run.no_reasoning.then_some(false),
        ..config::provider::OpenaiProvider::default()
    };
    providers::OpenAiCompatible::build(config::ProviderType::OpenaiCompatible, &settings)
        .map_err(|error| Failure::Provider(error.to_string()))
}

#[derive(Debug, sqlx::FromRow)]
struct LeafRow {
    id: Uuid,
    conversation_id: Option<Uuid>,
    topic: Option<Uuid>,
}

async fn leaves(pool: &PgPool, id: Uuid, secrets: &[String]) -> Result<Board, Failure> {
    let rows = sqlx::query_as::<_, LeafRow>(LEAVES)
        .bind(id)
        .fetch_all(pool)
        .await
        .map_err(|error| database(&error, secrets))?;
    let leaves = rows
        .into_iter()
        .map(|row| Leaf {
            id: row.id,
            conversation_id: row.conversation_id,
            topic: row.topic,
        })
        .collect::<Vec<_>>();
    let status = leaves.iter().map(|_| Status::Pending).collect();
    Ok(Board { leaves, status })
}

const LEAVES: &str = "\
SELECT s.id, s.conversation_id, p.topic_id AS topic \
FROM rh_summaries s \
JOIN rh_conversations c ON c.id = s.conversation_id \
LEFT JOIN rh_session_placements p ON p.session_id = c.session_key \
WHERE s.kind = 'leaf' AND s.conversation_id = $1 \
ORDER BY coalesce(s.earliest_at, s.created_at), s.id";

const ORDER: &str = "\
SELECT c.id, min(coalesce(s.earliest_at, s.created_at, c.created_at)) AS started \
FROM rh_conversations c \
LEFT JOIN rh_summaries s ON s.conversation_id = c.id AND s.kind = 'leaf' \
WHERE c.id = ANY($1) \
GROUP BY c.id";

const RETIRE: &str = "\
DELETE FROM graphile_worker._private_jobs j \
USING graphile_worker._private_tasks t \
WHERE t.id = j.task_id AND t.identifier IN ('extract-wiki', 'extract-entities') AND j.is_available";

struct Board {
    leaves: Vec<Leaf>,
    status: Vec<Status>,
}

enum Status {
    Pending,
    Held,
    Ok,
    Skipped,
    Failed(String),
}

struct Gate {
    board: Board,
    busy: HashSet<BusyKey>,
    pass: Pass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BusyKey {
    Topic(Uuid),
    Alone(Uuid),
    Conversation,
}

struct Batch {
    lead: Leaf,
    also: Vec<Leaf>,
    lead_at: usize,
    also_at: Vec<usize>,
    key: BusyKey,
    pass: Pass,
}

async fn drain(gate: Arc<Mutex<Gate>>, ctx: &Arc<Ctx>, concurrency: usize) -> Option<Failure> {
    let mut tasks = JoinSet::new();
    let mut failure = None;
    loop {
        if failure.is_none() {
            spawn_available(&mut tasks, &gate, ctx, concurrency).await;
        }
        if tasks.is_empty() {
            break;
        }
        match tasks.join_next().await {
            Some(Ok(Some(error))) if failure.is_none() => {
                failure = Some(error);
                tasks.abort_all();
            }
            Some(Err(error)) if failure.is_none() && !error.is_cancelled() => {
                failure = Some(Failure::Worker(error.to_string()));
                tasks.abort_all();
            }
            Some(Ok(_) | Err(_)) => {}
            None => break,
        }
    }
    failure
}

async fn spawn_available(
    tasks: &mut JoinSet<Option<Failure>>,
    gate: &Arc<Mutex<Gate>>,
    ctx: &Arc<Ctx>,
    concurrency: usize,
) {
    while tasks.len() < concurrency {
        let batch = {
            let mut guard = gate.lock().await;
            take(&mut guard)
        };
        let Some(batch) = batch else {
            return;
        };
        let gate = Arc::clone(gate);
        let ctx = Arc::clone(ctx);
        tasks.spawn(Scope::carry(async move { one(gate, ctx, batch).await }));
    }
}

async fn one(gate: Arc<Mutex<Gate>>, ctx: Arc<Ctx>, batch: Batch) -> Option<Failure> {
    let result = run_lead(&ctx, &batch.lead, &batch.also, batch.pass).await;
    let mut guard = gate.lock().await;
    guard.busy.remove(&batch.key);
    apply(&mut guard.board, &batch, result)
}

fn apply(board: &mut Board, batch: &Batch, result: Result<LeadEnd, Failure>) -> Option<Failure> {
    let LeadEnd { done, failed } = match result {
        Ok(end) => end,
        Err(error) => {
            note(&format!("harness {} {error}", batch.lead.id));
            mark(board, batch.lead_at, Status::Failed(error.to_string()));
            release_pending(board, batch, &HashSet::new());
            return Some(error);
        }
    };
    if let Some(error) = &failed {
        note(&format!("failed {} {error}", batch.lead.id));
        mark(board, batch.lead_at, Status::Failed(error.clone()));
    } else {
        note(&format!("ok {}", batch.lead.id));
        mark(board, batch.lead_at, Status::Ok);
    }
    release_pending(board, batch, &done);
    None
}

fn release_pending(board: &mut Board, batch: &Batch, done: &HashSet<Uuid>) {
    for (index, leaf) in batch.also_at.iter().zip(&batch.also) {
        let status = if done.contains(&leaf.id) {
            Status::Skipped
        } else {
            Status::Pending
        };
        mark(board, *index, status);
    }
}

fn take(gate: &mut Gate) -> Option<Batch> {
    let lead_at = gate
        .board
        .leaves
        .iter()
        .enumerate()
        .find_map(|(index, leaf)| {
            let pending = matches!(gate.board.status.get(index), Some(Status::Pending));
            (pending && !gate.busy.contains(&busy_key(gate, leaf))).then_some(index)
        })?;
    let lead = gate.board.leaves.get(lead_at)?.clone();
    let key = busy_key(gate, &lead);
    let mut also = Vec::new();
    let mut also_at = Vec::new();
    match gate.pass {
        Pass::Entities => collect_consecutive(gate, lead_at, &mut also, &mut also_at),
        Pass::Topics => {
            if let BusyKey::Topic(topic) = key {
                collect_also(gate, lead_at, topic, &mut also, &mut also_at);
            }
        }
    }
    mark(&mut gate.board, lead_at, Status::Held);
    for index in &also_at {
        mark(&mut gate.board, *index, Status::Held);
    }
    gate.busy.insert(key);
    Some(Batch {
        lead,
        also,
        lead_at,
        also_at,
        key,
        pass: gate.pass,
    })
}

fn collect_also(
    gate: &Gate,
    lead_at: usize,
    topic: Uuid,
    also: &mut Vec<Leaf>,
    also_at: &mut Vec<usize>,
) {
    let later = gate
        .board
        .leaves
        .iter()
        .enumerate()
        .skip(lead_at.saturating_add(1));
    for (index, leaf) in later {
        if also.len() == COALESCE_MORE {
            break;
        }
        let pending = matches!(gate.board.status.get(index), Some(Status::Pending));
        if pending && leaf.topic == Some(topic) {
            also.push(leaf.clone());
            also_at.push(index);
        }
    }
}

fn collect_consecutive(
    gate: &Gate,
    lead_at: usize,
    also: &mut Vec<Leaf>,
    also_at: &mut Vec<usize>,
) {
    let later = gate
        .board
        .leaves
        .iter()
        .enumerate()
        .skip(lead_at.saturating_add(1));
    for (index, leaf) in later {
        if also.len() == COALESCE_MORE {
            break;
        }
        let pending = matches!(gate.board.status.get(index), Some(Status::Pending));
        if !pending {
            break;
        }
        also.push(leaf.clone());
        also_at.push(index);
    }
}

fn busy_key(gate: &Gate, leaf: &Leaf) -> BusyKey {
    match gate.pass {
        Pass::Entities => BusyKey::Conversation,
        Pass::Topics => match leaf.topic {
            Some(topic) => BusyKey::Topic(topic),
            None => BusyKey::Alone(leaf.id),
        },
    }
}

fn mark(board: &mut Board, index: usize, status: Status) {
    if let Some(slot) = board.status.get_mut(index) {
        *slot = status;
    }
}

fn outcomes(board: &Board) -> Vec<JobOutcome> {
    board
        .leaves
        .iter()
        .enumerate()
        .map(|(index, leaf)| {
            let (outcome, error) = match board.status.get(index) {
                Some(Status::Ok) => ("ok", None),
                Some(Status::Skipped) => ("skipped", None),
                Some(Status::Failed(error)) => ("failed", Some(error.clone())),
                Some(Status::Pending | Status::Held) | None => ("not_run", None),
            };
            JobOutcome {
                summary_id: leaf.id,
                outcome,
                error,
            }
        })
        .collect()
}
