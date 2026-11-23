use jiff::Timestamp;
use kernel::flagged::FlaggedJobs;
use kernel::queue::ClaimSelector;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::queue::StoredJob;
use wire::JsonObject;

use crate::topics::Outcome;

#[derive(serde::Serialize)]
struct Body<'a> {
    #[serde(rename = "targetId")]
    target_id: &'a str,
}

pub async fn a_re_added_key_is_flagged_on_the_next_failure<S, Q, D>(
    store: &S,
    queue: &Q,
    mut due: D,
) -> Outcome
where
    S: FlaggedJobs,
    Q: JobQueue,
    D: AsyncFnMut() -> Outcome,
{
    queue
        .enqueue(embed("again", 1, JobKeyMode::Replace)?)
        .await?;
    fail_due(queue, &mut due, "first failure").await?;
    let first = only(store).await?;
    assert_eq!(first.failures, 1);
    assert_eq!(first.error, "first failure");
    queue
        .enqueue(embed("again", 3, JobKeyMode::Replace)?)
        .await?;
    fail_due(queue, &mut due, "second failure").await?;
    due().await?;
    assert_eq!(claim_embed(queue).await?, None);
    let again = only(store).await?;
    assert_eq!(again.key.as_deref(), Some("again"));
    assert_eq!(again.failures, 2);
    assert_eq!(again.error, "second failure");
    Ok(())
}

pub async fn failure_times_move_forward_and_a_completed_retry_drops_its_count<S, Q, D, C>(
    store: &S,
    queue: &Q,
    mut due: D,
    mut failure_rows: C,
) -> Outcome
where
    S: FlaggedJobs,
    Q: JobQueue,
    D: AsyncFnMut() -> Outcome,
    C: AsyncFnMut() -> Result<i64, Box<dyn std::error::Error>>,
{
    queue
        .enqueue(embed("counted", 25, JobKeyMode::Replace)?)
        .await?;
    fail_due(queue, &mut due, "slug conflicts with ros-1").await?;
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    for index in 2..=4 {
        fail_due(queue, &mut due, &format!("slug conflicts with ros-{index}")).await?;
    }
    fail_due(queue, &mut due, "slug conflicts with ros-5").await?;
    assert_eq!(claim_embed(queue).await?, None);
    let counted = only(store).await?;
    assert_eq!(counted.failures, 5);
    assert_eq!(counted.error, "slug conflicts with ros-5");
    let first = counted.first_failed_at.ok_or("first failure time")?;
    assert!(first < counted.last_failed_at);
    queue
        .enqueue(embed("kept", 1, JobKeyMode::Replace)?)
        .await?;
    fail_due(queue, &mut due, "kept failure").await?;
    store.retry(counted.id).await?;
    let claimed = claim_embed(queue).await?.ok_or("the retried job")?;
    queue.complete(claimed.id).await?;
    let kept = only(store).await?;
    store.dismiss(kept.id, Timestamp::now()).await?;
    assert_eq!(store.flagged().await?, Vec::new());
    assert_eq!(failure_rows().await?, 1);
    Ok(())
}

pub async fn a_dismissed_ledger_id_can_be_retried<S, Q, D>(
    store: &S,
    queue: &Q,
    mut due: D,
) -> Outcome
where
    S: FlaggedJobs,
    Q: JobQueue,
    D: AsyncFnMut() -> Outcome,
{
    queue
        .enqueue(embed("dismissed", 1, JobKeyMode::Replace)?)
        .await?;
    fail_due(queue, &mut due, "dismiss me").await?;
    let flagged = only(store).await?;
    store.dismiss(flagged.id, Timestamp::now()).await?;
    assert_eq!(store.flagged().await?, Vec::new());
    store.retry(flagged.id).await?;
    let claimed = claim_embed(queue)
        .await?
        .ok_or("the dismissed job was not retried")?;
    assert_eq!(claimed.name, JobName::Fixed(FixedJob::EmbedTarget));
    Ok(())
}

pub async fn replace_resets_attempts_and_the_maximum<S, Q, D>(
    store: &S,
    queue: &Q,
    mut due: D,
) -> Outcome
where
    S: FlaggedJobs,
    Q: JobQueue,
    D: AsyncFnMut() -> Outcome,
{
    let first = queue
        .enqueue(embed("reset", 4, JobKeyMode::Replace)?)
        .await?;
    fail_due(queue, &mut due, "still queued").await?;
    assert_eq!(store.flagged().await?, Vec::new());
    let stored = queue.read(first).await?.ok_or("the job left the queue")?;
    assert_eq!(stored.attempts, 1);
    let replaced = queue
        .enqueue(embed("reset", 9, JobKeyMode::Replace)?)
        .await?;
    let stored = queued_job(queue, replaced).await?;
    assert_eq!(stored.attempts, 0);
    assert_eq!(stored.max_attempts, 9);
    assert_eq!(stored.last_error, None);
    let mut other = embed("reset", 9, JobKeyMode::Replace)?;
    other.name = JobName::Fixed(FixedJob::SynthesizeToolCall);
    let renamed = queue.enqueue(other).await?;
    assert_eq!(
        queued_job(queue, renamed).await?.name,
        JobName::Fixed(FixedJob::SynthesizeToolCall)
    );
    Ok(())
}

pub async fn replace_detaches_a_job_that_is_still_running<Q, D>(queue: &Q, mut due: D) -> Outcome
where
    Q: JobQueue,
    D: AsyncFnMut() -> Outcome,
{
    let running = queue
        .enqueue(embed("held", 4, JobKeyMode::Replace)?)
        .await?;
    due().await?;
    claim_embed(queue).await?.ok_or("the running job")?;
    let fresh = queue
        .enqueue(embed("held", 9, JobKeyMode::Replace)?)
        .await?;
    let held = queued_job(queue, running).await?;
    assert_eq!(held.key, None);
    assert_eq!(held.attempts, 4);
    let next = queued_job(queue, fresh).await?;
    assert_eq!(next.key.as_deref(), Some("held"));
    assert_eq!(next.attempts, 0);
    assert_eq!(next.max_attempts, 9);
    Ok(())
}

pub fn ledger_job(key: &str, max_attempts: i32) -> Result<Job, serde_json::Error> {
    embed(key, max_attempts, JobKeyMode::Replace)
}

fn embed(key: &str, max_attempts: i32, mode: JobKeyMode) -> Result<Job, serde_json::Error> {
    Ok(Job {
        name: JobName::Fixed(FixedJob::EmbedTarget),
        payload: JsonObject::from_serialize(&Body { target_id: key })?,
        key: Some(key.to_owned()),
        key_mode: mode,
        priority: 0,
        run_at: Some(Timestamp::now()),
        max_attempts: Some(max_attempts),
    })
}

async fn fail_due<Q, D>(queue: &Q, due: &mut D, error: &str) -> Outcome
where
    Q: JobQueue,
    D: AsyncFnMut() -> Outcome,
{
    due().await?;
    let claimed = claim_embed(queue).await?.ok_or("a due job")?;
    queue.fail(claimed.id, error).await?;
    Ok(())
}

async fn claim_embed<Q: JobQueue>(
    queue: &Q,
) -> Result<Option<kernel::queue::ClaimedJob>, kernel::queue::JobQueueError> {
    queue
        .claim(&ClaimSelector::matching(vec![JobName::Fixed(
            FixedJob::EmbedTarget,
        )]))
        .await
}

async fn only<S: FlaggedJobs>(store: &S) -> Result<kernel::flagged::FlaggedJob, String> {
    let mut rows = store.flagged().await.map_err(|error| error.to_string())?;
    let job = rows.pop().ok_or("no flagged job")?;
    if rows.is_empty() {
        Ok(job)
    } else {
        Err(format!("{} flagged jobs", rows.len().saturating_add(1)))
    }
}

async fn queued_job<Q: JobQueue>(queue: &Q, id: kernel::queue::JobId) -> Result<StoredJob, String> {
    queue
        .read(id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("job {id} is missing"))
}
