use kernel::queue::{ClaimSelector, FixedJob, JobId, JobName};
use kernel::wiki::entity_pass::{EntityLeaf, EntityWork, ExtractEntities};

use super::EntityBuilderEnv;
use crate::WorkerError;

const COALESCE_MORE: u32 = 7;
const COALESCE_CHARS: usize = 24_000;

#[derive(Default)]
pub(super) struct Gathered {
    pub(super) also: Vec<EntityLeaf>,
    pub(super) claimed: Vec<JobId>,
}

pub(super) async fn gather(
    env: &EntityBuilderEnv<'_>,
    lead: &EntityLeaf,
) -> Result<Gathered, WorkerError> {
    let mut gathered = Gathered::default();
    if let Err(error) = gather_into(env, lead, &mut gathered).await {
        settle(env, &gathered.claimed, false).await;
        return Err(error);
    }
    Ok(gathered)
}

async fn gather_into(
    env: &EntityBuilderEnv<'_>,
    lead: &EntityLeaf,
    gathered: &mut Gathered,
) -> Result<(), WorkerError> {
    let following = env.store.following(lead, COALESCE_MORE).await?;
    let mut size = lead.content.len();
    for candidate in following {
        if env.cancel.is_cancelled() || size >= COALESCE_CHARS {
            break;
        }
        if candidate.done {
            break;
        }
        let Some(job) = candidate.job else {
            break;
        };
        let next = size.saturating_add(candidate.leaf.content.len());
        if next > COALESCE_CHARS {
            break;
        }
        if !claim(env, job).await? {
            break;
        }
        gathered.claimed.push(job);
        match take(env, &candidate.leaf).await? {
            Taken::Joins(joined) => {
                size = size.saturating_add(joined.content.len());
                gathered.also.push(joined);
            }
            Taken::Stop => {
                gathered.claimed.pop();
                env.queue.complete(job).await.map_err(WorkerError::Queue)?;
                break;
            }
        }
    }
    Ok(())
}

async fn claim(env: &EntityBuilderEnv<'_>, job: JobId) -> Result<bool, WorkerError> {
    let selector = ClaimSelector {
        names: vec![JobName::Fixed(FixedJob::ExtractEntities)],
        only_id: Some(job),
    };
    Ok(env
        .queue
        .claim(&selector)
        .await
        .map_err(WorkerError::Queue)?
        .is_some())
}

enum Taken {
    Joins(EntityLeaf),
    Stop,
}

async fn take(env: &EntityBuilderEnv<'_>, leaf: &EntityLeaf) -> Result<Taken, WorkerError> {
    let job = ExtractEntities {
        summary_id: leaf.id,
        conversation_id: leaf.conversation_id,
        session_at: None,
    };
    match env.store.entity_work(&job).await? {
        EntityWork::Complete { .. } => Ok(Taken::Stop),
        EntityWork::Pending(fresh) => Ok(Taken::Joins(fresh)),
    }
}

pub(super) async fn settle(env: &EntityBuilderEnv<'_>, claimed: &[JobId], applied: bool) {
    for id in claimed {
        let settled = if applied {
            env.queue.complete(*id).await
        } else {
            env.queue.release(*id).await
        };
        if let Err(error) = settled {
            tracing::warn!("extract-entities: coalesced job {id} was not settled: {error}");
        }
    }
}
