use jiff::Timestamp;
use kernel::queue::ClaimSelector;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::queue::WorkerRole;
use wire::JsonObject;

use crate::topics::Outcome;
use crate::topics::same;

const BACKLOG: usize = 3;

pub async fn a_pending_sweep_is_queued_once(queue: &dyn JobQueue) -> Outcome {
    let idle = JobName::Fixed(FixedJob::EnqueueIdle);
    let entry = WorkerRole::Summarizer
        .crontab(false)
        .into_iter()
        .find(|entry| entry.task == idle)
        .ok_or("the summarizer schedules no idle sweep")?;
    queue.enqueue(entry.job()).await?;
    queue.enqueue(entry.job()).await?;
    let selector = ClaimSelector::matching(vec![idle]);
    let claimed = queue.claim(&selector).await?.ok_or("no sweep is pending")?;
    queue.complete(claimed.id).await?;
    assert_eq!(queue.claim(&selector).await?, None);
    Ok(())
}

pub async fn the_sweeps_run_before_an_older_backlog(queue: &dyn JobQueue) -> Outcome {
    for (role, work) in [
        (WorkerRole::Summarizer, FixedJob::SummarizeConversation),
        (WorkerRole::Embedder, FixedJob::EmbedTarget),
        (WorkerRole::Categorizer, FixedJob::FileConversation),
        (WorkerRole::WikiBuilder, FixedJob::ExtractWiki),
    ] {
        sweeps_before_backlog(queue, role, work).await?;
    }
    Ok(())
}

async fn sweeps_before_backlog(queue: &dyn JobQueue, role: WorkerRole, work: FixedJob) -> Outcome {
    let queued = Timestamp::now().checked_sub(wire::time::elapsed_days(19))?;
    for index in 0..BACKLOG {
        queue
            .enqueue(Job {
                name: JobName::Fixed(work),
                payload: JsonObject::default(),
                key: Some(format!("{work}-backlog-{index}")),
                key_mode: JobKeyMode::Replace,
                priority: 0,
                run_at: Some(queued),
                max_attempts: Some(3),
            })
            .await?;
    }
    let names = queue.task_names(role);
    let entries: Vec<_> = role
        .crontab(true)
        .into_iter()
        .filter(|entry| names.contains(&entry.task))
        .collect();
    for entry in &entries {
        queue.enqueue(entry.job()).await?;
    }
    let mut sweeps: Vec<String> = entries.iter().map(|entry| entry.task.to_string()).collect();
    sweeps.sort();
    let selector = ClaimSelector::matching(names);
    let mut order = Vec::new();
    while let Some(claimed) = queue.claim(&selector).await? {
        order.push(claimed.name.to_string());
        queue.complete(claimed.id).await?;
    }
    let (first, last) = order.split_at(sweeps.len().min(order.len()));
    let mut first = first.to_vec();
    first.sort();
    same("the jobs claimed first", first, sweeps)?;
    same(
        "the jobs claimed last",
        last.to_vec(),
        vec![work.to_string(); BACKLOG],
    )?;
    Ok(())
}
