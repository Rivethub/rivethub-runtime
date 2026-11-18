use jiff::Timestamp;
use kernel::memory::jobs::{
    ChunkWrite, MemoryJobError, MemoryJobStore, UnchunkedMessage, UnembeddedStore,
};
use kernel::memory::{EmbedTable, EmbedTarget};
use kernel::queue::{FixedJob, Job, JobKeyMode, JobName, JobQueue};
use tokio_util::sync::CancellationToken;

use crate::{WorkerError, compose_message, embed_key, split_embed_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WikiSweep {
    pub revived: u64,
    pub added: usize,
}

pub async fn handle_enqueue_unembedded(
    store: &dyn UnembeddedStore,
    queue: &dyn JobQueue,
    limit: u32,
    max_attempts: u32,
    cancel: &CancellationToken,
) -> Result<usize, WorkerError> {
    let mut enqueued = 0;
    for table in [
        EmbedTable::Messages,
        EmbedTable::Summaries,
        EmbedTable::WikiTopics,
        EmbedTable::MessageChunks,
    ] {
        let targets = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(WorkerError::Cancelled),
            result = store.unembedded(table, limit) => result,
        };
        match targets {
            Ok(targets) => enqueued += enqueue(queue, targets, max_attempts, cancel).await?,
            Err(MemoryJobError::MissingTable { source, .. }) => {
                tracing::warn!("[enqueue-unembedded] {table} sweep failed: {source}");
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(enqueued)
}

pub(crate) async fn enqueue(
    queue: &dyn JobQueue,
    targets: Vec<EmbedTarget>,
    max_attempts: u32,
    cancel: &CancellationToken,
) -> Result<usize, WorkerError> {
    let count = targets.len();
    for target in targets {
        let job = embed_sweep_job(&target, max_attempts)?;
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(WorkerError::Cancelled),
            result = queue.enqueue(job) => { result.map_err(WorkerError::Queue)?; }
        }
    }
    Ok(count)
}

pub fn embed_sweep_job(target: &EmbedTarget, max_attempts: u32) -> Result<Job, WorkerError> {
    Ok(Job {
        name: JobName::Fixed(FixedJob::EmbedTarget),
        payload: crate::embed::embed_payload(target)?,
        key: Some(embed_key(target)),
        key_mode: JobKeyMode::KeepRunAt,
        priority: 0,
        run_at: None,
        max_attempts: Some(i32::try_from(max_attempts).map_err(|_| WorkerError::Count)?),
    })
}

pub async fn handle_enqueue_unchunked(
    store: &dyn MemoryJobStore,
    queue: &dyn JobQueue,
    settings: crate::EmbedSettings,
    limit: u32,
    max_attempts: u32,
    cancel: &CancellationToken,
) -> Result<usize, WorkerError> {
    if !settings.chunks_enabled {
        return Ok(0);
    }
    let selected = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(WorkerError::Cancelled),
        result = store.unchunked(limit, settings.chars_per_chunk, settings.tool_result_cap) => result,
    };
    let messages = match selected {
        Ok(messages) => messages,
        Err(error) => {
            tracing::warn!("[enqueue-unchunked] sweep failed: {error}");
            return Ok(0);
        }
    };
    let mut enqueued = 0;
    for message in &messages {
        match chunk_message(store, queue, settings, message, max_attempts, cancel).await {
            Ok(count) => enqueued += count,
            Err(WorkerError::Cancelled) => return Err(WorkerError::Cancelled),
            Err(error) => {
                tracing::warn!("[enqueue-unchunked] message {} failed: {error}", message.id);
            }
        }
    }
    Ok(enqueued)
}

async fn chunk_message(
    store: &dyn MemoryJobStore,
    queue: &dyn JobQueue,
    settings: crate::EmbedSettings,
    message: &UnchunkedMessage,
    max_attempts: u32,
    cancel: &CancellationToken,
) -> Result<usize, WorkerError> {
    let cap = usize::try_from(settings.tool_result_cap).map_err(|_| WorkerError::TextTooLong)?;
    let window = usize::try_from(settings.chars_per_chunk).map_err(|_| WorkerError::TextTooLong)?;
    let text = compose_message(&message.content, message.tool_result.as_deref(), cap);
    if text.chars().count() <= window {
        return Ok(0);
    }
    let chunks = split_embed_text(&text, window)?
        .iter()
        .enumerate()
        .map(|(index, piece)| crate::embed::stored_chunk(index, piece, None))
        .collect::<Result<Vec<_>, _>>()?;
    let written = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(WorkerError::Cancelled),
        result = store.write_chunks(message.id, chunks) => result?,
    };
    if written == ChunkWrite::Skipped {
        return Ok(0);
    }
    let targets = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(WorkerError::Cancelled),
        result = store.unembedded_chunks(message.id) => result?,
    };
    enqueue(queue, targets, max_attempts, cancel).await
}

pub async fn handle_enqueue_stale_wiki(
    store: &dyn MemoryJobStore,
    queue: &dyn JobQueue,
    enabled: bool,
    cap: u32,
    cancel: &CancellationToken,
) -> Result<WikiSweep, WorkerError> {
    let mut sweep = WikiSweep {
        revived: 0,
        added: 0,
    };
    if !enabled || cap == 0 {
        return Ok(sweep);
    }
    sweep.revived = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(WorkerError::Cancelled),
        result = store.revive_dead_wiki(cap) => result?,
    };
    if sweep.revived > 0 {
        tracing::info!(
            "[reschedule-dead] revived {} dead extract-wiki job(s)",
            sweep.revived
        );
    }
    let left = u64::from(cap)
        .checked_sub(sweep.revived)
        .ok_or(WorkerError::Count)?;
    let missing_cap = u32::try_from(left).map_err(|_| WorkerError::Count)?;
    if missing_cap == 0 {
        return Ok(sweep);
    }
    let jobs = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(WorkerError::Cancelled),
        result = store.missing_wiki(missing_cap) => result?,
    };
    let now = Timestamp::now();
    for job in &jobs {
        let added = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(WorkerError::Cancelled),
            result = queue.enqueue(crate::wiki_builder::missing_wiki_job(job, now)?) => result,
        };
        if let Err(error) = added {
            tracing::warn!(
                "[enqueue-stale-wiki] addJob failed after re-adding {}/{} missing job(s)",
                sweep.added,
                jobs.len()
            );
            return Err(WorkerError::Queue(error));
        }
        sweep.added += 1;
    }
    if sweep.added > 0 {
        tracing::info!(
            "[enqueue-stale-wiki] re-added {} missing job(s)",
            sweep.added
        );
    }
    Ok(sweep)
}
