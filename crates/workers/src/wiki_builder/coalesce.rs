use kernel::queue::{ClaimSelector, FixedJob, JobId, JobName};
use kernel::wiki::builder::{ExtractWiki, WikiSummary, WikiWork};
use kernel::wiki::route::{TopicTarget, WikiRouter};

use super::WikiBuilderEnv;
use crate::WorkerError;

const COALESCE_MORE: u32 = 7;
const COALESCE_CHARS: usize = 24_000;

#[derive(Default)]
pub(super) struct Gathered {
    pub(super) also: Vec<WikiSummary>,
    pub(super) claimed: Vec<JobId>,
}

pub(super) async fn gather(
    env: &WikiBuilderEnv<'_>,
    router: &WikiRouter<'_>,
    lead: &WikiSummary,
    target: &TopicTarget,
) -> Result<Gathered, WorkerError> {
    let mut gathered = Gathered::default();
    if let Err(error) = gather_into(env, router, lead, target, &mut gathered).await {
        settle(env, &gathered.claimed, false).await;
        return Err(error);
    }
    Ok(gathered)
}

async fn gather_into(
    env: &WikiBuilderEnv<'_>,
    router: &WikiRouter<'_>,
    lead: &WikiSummary,
    target: &TopicTarget,
    gathered: &mut Gathered,
) -> Result<(), WorkerError> {
    let candidates = env
        .store
        .coalescable(lead.id, target.topic, COALESCE_MORE)
        .await?;
    let mut size = lead.content.len();
    for candidate in candidates {
        if env.cancel.is_cancelled() || size >= COALESCE_CHARS {
            break;
        }
        let selector = ClaimSelector {
            names: vec![JobName::Fixed(FixedJob::ExtractWiki)],
            only_id: Some(candidate.job),
        };
        let free = env
            .queue
            .claim(&selector)
            .await
            .map_err(WorkerError::Queue)?;
        if free.is_none() {
            continue;
        }
        gathered.claimed.push(candidate.job);
        match take(env, router, target, &candidate.extract, size).await? {
            Taken::Joins(summary) => {
                size = size.saturating_add(summary.content.len());
                gathered.also.push(summary);
            }
            Taken::Done => {
                gathered.claimed.pop();
                env.queue
                    .complete(candidate.job)
                    .await
                    .map_err(WorkerError::Queue)?;
            }
            Taken::Apart => {
                gathered.claimed.pop();
                env.queue
                    .release(candidate.job)
                    .await
                    .map_err(WorkerError::Queue)?;
            }
        }
    }
    Ok(())
}

enum Taken {
    Joins(WikiSummary),
    Done,
    Apart,
}

async fn take(
    env: &WikiBuilderEnv<'_>,
    router: &WikiRouter<'_>,
    target: &TopicTarget,
    extract: &ExtractWiki,
    size: usize,
) -> Result<Taken, WorkerError> {
    let work = match env.store.wiki_work(extract).await {
        Err(error) if error.obsolete_input() => return Ok(Taken::Done),
        result => result?,
    };
    let summary = match work {
        WikiWork::Complete { .. } => return Ok(Taken::Done),
        WikiWork::Pending(summary) => summary,
    };
    let fits = size.saturating_add(summary.content.len()) <= COALESCE_CHARS;
    if fits && router.same_route(target, &summary).await? {
        Ok(Taken::Joins(summary))
    } else {
        Ok(Taken::Apart)
    }
}

pub(super) async fn settle(env: &WikiBuilderEnv<'_>, claimed: &[JobId], applied: bool) {
    for id in claimed {
        let settled = if applied {
            env.queue.complete(*id).await
        } else {
            env.queue.release(*id).await
        };
        if let Err(error) = settled {
            tracing::warn!("extract-wiki: coalesced job {id} was not settled: {error}");
        }
    }
}
