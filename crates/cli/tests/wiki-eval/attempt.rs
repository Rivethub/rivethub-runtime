use std::collections::HashSet;
use std::time::Duration;

use kernel::queue::ClaimSelector;
use kernel::queue::FixedJob;
use kernel::queue::JobId;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::wiki::builder::ExtractWiki;
use kernel::wiki::route::TopicPorts;
use memory_postgres::PostgresMemory;
use memory_postgres::PostgresQueue;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::JsonObject;
use workers::WorkerError;
use workers::wiki_builder::WikiBuilderEnv;

use crate::failure::Failure;
use crate::failure::redact;
use crate::lineage::EmptyLineage;
use crate::logged::LoggedProvider;

const OUTAGE_CAP: u32 = 5;
const BUSY_CAP: u32 = 3;
const HARD_CAP: u32 = 2;
const BUSY_DELAY: Duration = Duration::from_secs(30);

pub(crate) struct Ctx {
    pub(crate) memory: std::sync::Arc<PostgresMemory>,
    pub(crate) queue: std::sync::Arc<PostgresQueue>,
    pub(crate) provider: std::sync::Arc<LoggedProvider>,
    pub(crate) lineage: EmptyLineage,
    pub(crate) model: String,
    pub(crate) cancel: CancellationToken,
    pub(crate) secrets: Vec<String>,
    pub(crate) matcher: workers::entity_builder::MatcherSettings,
}

#[derive(Debug, Clone)]
pub(crate) struct Leaf {
    pub(crate) id: Uuid,
    pub(crate) conversation_id: Option<Uuid>,
    pub(crate) topic: Option<Uuid>,
}

pub(crate) struct LeadEnd {
    pub(crate) done: HashSet<Uuid>,
    pub(crate) failed: Option<String>,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(sleep, reason = "the wait is the fixture")
)]
pub(crate) async fn run_lead(
    ctx: &Ctx,
    lead: &Leaf,
    also: &[Leaf],
    pass: crate::drive::Pass,
) -> Result<LeadEnd, Failure> {
    for sibling in also {
        enqueue(ctx, sibling, pass).await?;
    }
    let (id, payload) = enqueue(ctx, lead, pass).await?;
    let mut locked = claim(ctx, id, pass).await?;
    let mut outages = 0u32;
    let mut busy = 0u32;
    let mut hard = 0u32;
    let mut delay = Duration::from_secs(1);
    loop {
        match dispatch(ctx, &payload, pass).await {
            Ok(()) => return finish_ok(ctx, locked, also, pass).await,
            Err(error) => {
                let step = classify(&error);
                let message = redact(&error.to_string(), &ctx.secrets);
                match step {
                    Step::Outage => {
                        if stop(&mut outages, OUTAGE_CAP) {
                            return stop_lead(ctx, locked, also, message, pass).await;
                        }
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(Duration::from_secs(60));
                    }
                    Step::Busy => {
                        if stop(&mut busy, BUSY_CAP) {
                            return stop_lead(ctx, locked, also, message, pass).await;
                        }
                        ctx.queue
                            .postpone(locked, BUSY_DELAY)
                            .await
                            .map_err(|error| {
                                Failure::Queue(redact(&error.to_string(), &ctx.secrets))
                            })?;
                        tokio::time::sleep(BUSY_DELAY).await;
                        locked = claim(ctx, id, pass).await?;
                    }
                    Step::Hard => {
                        give_up(ctx, locked, &message).await?;
                        if stop(&mut hard, HARD_CAP) {
                            return stopped(ctx, also, message, pass).await;
                        }
                        locked = claim(ctx, id, pass).await?;
                    }
                }
            }
        }
    }
}

enum Step {
    Outage,
    Busy,
    Hard,
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every worker error other than an outage or a busy route fails the job the way production settle does"
)]
fn classify(error: &WorkerError) -> Step {
    match error {
        WorkerError::ProviderOutage { .. } => Step::Outage,
        WorkerError::RouteBusy => Step::Busy,
        _ => Step::Hard,
    }
}

fn stop(count: &mut u32, cap: u32) -> bool {
    *count = count.saturating_add(1);
    *count >= cap
}

async fn dispatch(
    ctx: &Ctx,
    payload: &JsonObject,
    pass: crate::drive::Pass,
) -> Result<(), WorkerError> {
    let env = WikiBuilderEnv {
        store: ctx.memory.as_ref(),
        provider: ctx.provider.as_ref(),
        queue: ctx.queue.as_ref(),
        model: &ctx.model,
        now: chrono::Utc::now(),
        cancel: &ctx.cancel,
        topics: Some(TopicPorts {
            topics: ctx.memory.as_ref(),
            pages: ctx.memory.as_ref(),
            filing: ctx.memory.as_ref(),
            lineage: &ctx.lineage,
        }),
    };
    match pass {
        crate::drive::Pass::Topics => workers::wiki_builder::handle_extract_wiki(&env, payload)
            .await
            .map(|_| ()),
        crate::drive::Pass::Entities => {
            let entity = workers::entity_builder::EntityBuilderEnv {
                store: ctx.memory.as_ref(),
                provider: ctx.provider.as_ref(),
                queue: ctx.queue.as_ref(),
                model: &ctx.model,
                settings: &ctx.matcher,
                now: chrono::Utc::now(),
                cancel: &ctx.cancel,
            };
            workers::entity_builder::handle_extract_entities(&entity, payload)
                .await
                .map(|_| ())
        }
    }
}

async fn finish_ok(
    ctx: &Ctx,
    locked: JobId,
    also: &[Leaf],
    pass: crate::drive::Pass,
) -> Result<LeadEnd, Failure> {
    ctx.queue
        .complete(locked)
        .await
        .map_err(|error| Failure::Queue(redact(&error.to_string(), &ctx.secrets)))?;
    Ok(LeadEnd {
        done: done_ids(ctx.memory.pool(), also, &ctx.secrets, pass).await?,
        failed: None,
    })
}

async fn stop_lead(
    ctx: &Ctx,
    locked: JobId,
    also: &[Leaf],
    message: String,
    pass: crate::drive::Pass,
) -> Result<LeadEnd, Failure> {
    give_up(ctx, locked, &message).await?;
    stopped(ctx, also, message, pass).await
}

async fn give_up(ctx: &Ctx, locked: JobId, message: &str) -> Result<(), Failure> {
    ctx.queue
        .fail(locked, message)
        .await
        .map_err(|error| Failure::Queue(redact(&error.to_string(), &ctx.secrets)))
}

async fn stopped(
    ctx: &Ctx,
    also: &[Leaf],
    message: String,
    pass: crate::drive::Pass,
) -> Result<LeadEnd, Failure> {
    Ok(LeadEnd {
        done: done_ids(ctx.memory.pool(), also, &ctx.secrets, pass).await?,
        failed: Some(message),
    })
}

async fn enqueue(
    ctx: &Ctx,
    leaf: &Leaf,
    pass: crate::drive::Pass,
) -> Result<(JobId, JsonObject), Failure> {
    let queued = match pass {
        crate::drive::Pass::Topics => workers::wiki_builder::extract_wiki_job(&ExtractWiki {
            summary_id: leaf.id,
            conversation_id: leaf.conversation_id,
        }),
        crate::drive::Pass::Entities => workers::entity_builder::extract_entities_job(
            &kernel::wiki::entity_pass::ExtractEntities {
                summary_id: leaf.id,
                conversation_id: leaf.conversation_id,
                session_at: None,
            },
        ),
    }
    .map_err(|error| Failure::Worker(redact(&error.to_string(), &ctx.secrets)))?;
    let payload = queued.payload.clone();
    let id = ctx
        .queue
        .enqueue(queued)
        .await
        .map_err(|error| Failure::Queue(redact(&error.to_string(), &ctx.secrets)))?;
    Ok((id, payload))
}

async fn claim(ctx: &Ctx, id: JobId, pass: crate::drive::Pass) -> Result<JobId, Failure> {
    let name = match pass {
        crate::drive::Pass::Topics => FixedJob::ExtractWiki,
        crate::drive::Pass::Entities => FixedJob::ExtractEntities,
    };
    let selector = ClaimSelector {
        names: vec![JobName::Fixed(name)],
        only_id: Some(id),
    };
    ctx.queue
        .claim(&selector)
        .await
        .map_err(|error| Failure::Queue(redact(&error.to_string(), &ctx.secrets)))?
        .map(|claimed| claimed.id)
        .ok_or_else(|| Failure::Queue(format!("{name} job {id} was not claimed")))
}

#[derive(Debug, sqlx::FromRow)]
struct DoneRow {
    summary_id: Uuid,
}

async fn done_ids(
    pool: &PgPool,
    also: &[Leaf],
    secrets: &[String],
    pass: crate::drive::Pass,
) -> Result<HashSet<Uuid>, Failure> {
    let ids: Vec<Uuid> = also.iter().map(|leaf| leaf.id).collect();
    let sql = match pass {
        crate::drive::Pass::Topics => {
            "SELECT summary_id FROM rh_wiki_extractions WHERE status = 'done' AND summary_id = ANY($1)"
        }
        crate::drive::Pass::Entities => {
            "SELECT summary_id FROM rh_entity_extractions WHERE status = 'done' AND summary_id = ANY($1)"
        }
    };
    let rows = sqlx::query_as::<_, DoneRow>(sql)
        .bind(&ids)
        .fetch_all(pool)
        .await
        .map_err(|error| Failure::Database(redact(&error.to_string(), secrets)))?;
    Ok(rows.into_iter().map(|row| row.summary_id).collect())
}
