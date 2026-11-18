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
