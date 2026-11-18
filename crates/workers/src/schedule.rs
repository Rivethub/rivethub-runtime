use jiff::Timestamp;
use kernel::memory::schedule::CronEntry;
use kernel::queue::{JobQueue, WorkerRole};
use tokio_util::sync::CancellationToken;
use wire::time::TimestampFormat as _;

use crate::WorkerError;

pub async fn enqueue_due(
    queue: &dyn JobQueue,
    entries: &[CronEntry],
    now: Timestamp,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let due = entries.iter().filter(|entry| {
        u32::from(
            jiff::tz::TimeZone::UTC
                .to_datetime(now)
                .minute()
                .unsigned_abs(),
        )
        .is_multiple_of(entry.every_minutes)
    });
    enqueue(queue, due, cancel).await
}

async fn enqueue(
    queue: &dyn JobQueue,
    entries: impl Iterator<Item = &CronEntry>,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    for entry in entries {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Ok(()),
            result = queue.enqueue(entry.job()) => { result.map_err(WorkerError::Queue)?; }
        }
    }
    Ok(())
}

#[must_use]
pub fn claimed_entries(
    queue: &dyn JobQueue,
    roles: &[WorkerRole],
    wiki_enabled: bool,
) -> Vec<CronEntry> {
    roles
        .iter()
        .flat_map(|role| {
            let claimed = queue.task_names(*role);
            role.crontab(wiki_enabled)
                .into_iter()
                .filter(move |entry| claimed.contains(&entry.task))
        })
        .collect()
}

pub(crate) async fn run(
    queue: &dyn JobQueue,
    roles: &[WorkerRole],
    wiki_enabled: bool,
    at_start: bool,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let entries = claimed_entries(queue, roles, wiki_enabled);
    if at_start {
        enqueue(queue, entries.iter(), cancel).await?;
    }
    loop {
        let now = Timestamp::now();
        let millis = 60_000 - now.epoch_millis().rem_euclid(60_000);
        let wait = std::time::Duration::from_millis(
            u64::try_from(millis).map_err(|_| WorkerError::Count)?,
        );
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Ok(()),
            () = tokio::time::sleep(wait) => {}
        }
        enqueue_due(queue, &entries, Timestamp::now(), cancel).await?;
    }
}

pub(crate) async fn run_summary_expiry(
    store: &dyn kernel::memory::SummaryStore,
    inactivity: jiff::SignedDuration,
    cancel: &CancellationToken,
) {
    let mut ticks = tokio::time::interval(std::time::Duration::from_secs(5 * 60));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            _ = ticks.tick() => {}
        }
        let now = Timestamp::now();
        let Ok(cutoff) = now.checked_sub(inactivity) else {
            tracing::error!("summary inactivity cutoff is outside the supported date range");
            return;
        };
        let mut after = None;
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let page = kernel::tenancy::Scope::Maintenance
                .run(store.expire_summary_page(
                    after,
                    cutoff,
                    now,
                    kernel::memory::settlement::SETTLEMENT_PAGE_SIZE,
                ))
                .await;
            match page {
                Ok(page) if page.next.is_some() => after = page.next,
                Ok(_) => break,
                Err(error) => {
                    tracing::warn!("summary settlement sweep failed: {error}");
                    break;
                }
            }
        }
    }
}

pub(crate) async fn run_summary_discovery(
    store: &dyn kernel::memory::SummaryStore,
    queue: &dyn JobQueue,
    cancel: &CancellationToken,
) {
    let mut ticks = tokio::time::interval(std::time::Duration::from_secs(5));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            _ = ticks.tick() => {}
        }
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            result = discover_summaries(store, queue) => result,
        };
        if let Err(error) = result {
            tracing::warn!("summary discovery failed: {error}");
        }
    }
}

async fn discover_summaries(
    store: &dyn kernel::memory::SummaryStore,
    queue: &dyn JobQueue,
) -> Result<(), WorkerError> {
    use crate::summarize::live::{ATTEMPTS, LiveRequest};
    use kernel::queue::{FixedJob, Job, JobKeyMode, JobName};
    let mut after = uuid::Uuid::nil();
    loop {
        let rows = store.live_candidates(after, 200).await?;
        if rows.is_empty() {
            return Ok(());
        }
        for (id, revision) in rows {
            let request = LiveRequest {
                conversation_id: id,
                revision: Some(revision),
            };
            queue
                .enqueue(Job {
                    name: JobName::Fixed(FixedJob::SummarizeConversation),
                    payload: wire::JsonObject::from_serialize(&request)
                        .map_err(WorkerError::summary_payload)?,
                    key: Some(format!("compact-{id}")),
                    key_mode: JobKeyMode::Arm,
                    priority: -20,
                    run_at: None,
                    max_attempts: Some(ATTEMPTS),
                })
                .await
                .map_err(WorkerError::queue)?;
            after = id;
        }
    }
}
