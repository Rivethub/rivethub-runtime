use crate::common;

use kernel::queue::{ClaimSelector, JobName, JobQueue, WorkerRole};
use memory_postgres::PostgresQueue;
use sqlx::PgPool;
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn add(pool: &PgPool, queue: Option<&str>) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT (graphile_worker.add_job('embed-target', '{}'::json, $1)).id")
        .bind(queue)
        .fetch_one(pool)
        .await
}

async fn owner(pool: &PgPool, queue: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT locked_by FROM graphile_worker._private_job_queues WHERE queue_name = $1",
    )
    .bind(queue)
    .fetch_one(pool)
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn concurrent_workers_serialize_each_named_queue_and_release_on_completion(
    pool: PgPool,
) -> TestResult {
    common::install_graphile(&pool).await?;
    let first = add(&pool, Some("serial")).await?;
    let second = add(&pool, Some("serial")).await?;
    let left = PostgresQueue::connect(pool.clone()).await?;
    let right = PostgresQueue::connect(pool.clone()).await?;
    let left_selector = ClaimSelector::matching(left.task_names(WorkerRole::Embedder));
    let right_selector = ClaimSelector::matching(right.task_names(WorkerRole::Embedder));
    let (a, b) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(left.claim(&left_selector), right.claim(&right_selector))
    })
    .await?;
    let (winner, loser, job) = match (a?, b?) {
        (Some(job), None) => (&left, &right, job),
        (None, Some(job)) => (&right, &left, job),
        other => return Err(format!("one queue has exactly one owner: {other:?}").into()),
    };
    assert_eq!(i64::from(job.id), first);
    let locked = owner(&pool, "serial").await?.ok_or("queue lock missing")?;
    let job_owner: String =
        sqlx::query_scalar("SELECT locked_by FROM graphile_worker._private_jobs WHERE id = $1")
            .bind(first)
            .fetch_one(&pool)
            .await?;
    assert_eq!(locked, job_owner);
    loser.complete(job.id).await?;
    loser.fail(job.id, "foreign worker").await?;
    assert_eq!(owner(&pool, "serial").await?, Some(locked));
    assert!(
        loser
            .claim(&kernel::queue::ClaimSelector::matching(
                loser.task_names(WorkerRole::Embedder)
            ))
            .await?
            .is_none()
    );
    winner.complete(job.id).await?;
    assert_eq!(owner(&pool, "serial").await?, None);
    let next = loser
        .claim(&kernel::queue::ClaimSelector::matching(
            loser.task_names(WorkerRole::Embedder),
        ))
        .await?
        .ok_or("second job")?;
    assert_eq!(i64::from(next.id), second);
    loser.complete(next.id).await?;
    assert_eq!(owner(&pool, "serial").await?, None);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn failure_unlocks_the_queue_and_preserves_retry_metadata(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let first = add(&pool, Some("serial")).await?;
    let second = add(&pool, Some("serial")).await?;
    let queue = PostgresQueue::connect(pool.clone()).await?;
    let job = queue
        .claim(&kernel::queue::ClaimSelector::matching(
            queue.task_names(WorkerRole::Embedder),
        ))
        .await?
        .ok_or("first job")?;
    queue.fail(job.id, "retry me").await?;
    assert_eq!(owner(&pool, "serial").await?, None);
    let state: (i16, Option<String>, Option<String>, bool) = sqlx::query_as(
        "SELECT attempts, last_error, locked_by, run_at > now() FROM graphile_worker._private_jobs WHERE id = $1",
    ).bind(first).fetch_one(&pool).await?;
    assert_eq!(state, (1, Some("retry me".to_owned()), None, true));
    let next = queue
        .claim(&kernel::queue::ClaimSelector::matching(
            queue.task_names(WorkerRole::Embedder),
        ))
        .await?
        .ok_or("second job")?;
    assert_eq!(i64::from(next.id), second);
    queue.complete(next.id).await?;
    sqlx::query("UPDATE graphile_worker._private_jobs SET run_at = now() WHERE id = $1")
        .bind(first)
        .execute(&pool)
        .await?;
    let retry = queue
        .claim(&kernel::queue::ClaimSelector::matching(
            queue.task_names(WorkerRole::Embedder),
        ))
        .await?
        .ok_or("retry")?;
    assert_eq!(retry.attempt, 2);
    queue.complete(retry.id).await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn locked_queues_do_not_block_other_queues_or_unnamed_jobs(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    add(&pool, Some("busy")).await?;
    let independent = add(&pool, Some("independent")).await?;
    let unnamed = add(&pool, None).await?;
    let mut lock = pool.begin().await?;
    sqlx::query(
        "SELECT id FROM graphile_worker._private_job_queues WHERE queue_name = 'busy' FOR UPDATE",
    )
    .execute(&mut *lock)
    .await?;
    let queue = PostgresQueue::connect(pool.clone()).await?;
    let first = tokio::time::timeout(
        Duration::from_secs(10),
        queue.claim(&kernel::queue::ClaimSelector::matching(
            queue.task_names(WorkerRole::Embedder),
        )),
    )
    .await??
    .ok_or("independent queue")?;
    assert_eq!(i64::from(first.id), independent);
    let second = queue
        .claim(&kernel::queue::ClaimSelector::matching(
            queue.task_names(WorkerRole::Embedder),
        ))
        .await?
        .ok_or("unnamed job")?;
    assert_eq!(i64::from(second.id), unnamed);
    assert!(
        queue
            .claim(&kernel::queue::ClaimSelector::matching(
                queue.task_names(WorkerRole::Embedder)
            ))
            .await?
            .is_none()
    );
    lock.rollback().await?;
    assert!(
        queue
            .claim(&kernel::queue::ClaimSelector::matching(
                queue.task_names(WorkerRole::Embedder)
            ))
            .await?
            .is_some()
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn expired_locks_are_reclaimed_and_old_workers_cannot_release_the_new_owner(
    pool: PgPool,
) -> TestResult {
    common::install_graphile(&pool).await?;
    add(&pool, Some("serial")).await?;
    let old = PostgresQueue::connect(pool.clone()).await?;
    let new = PostgresQueue::connect(pool.clone()).await?;
    let claimed = old
        .claim(&kernel::queue::ClaimSelector::matching(
            old.task_names(WorkerRole::Embedder),
        ))
        .await?
        .ok_or("old claim")?;
    sqlx::query("UPDATE graphile_worker._private_jobs SET locked_at = now() - interval '5 hours'")
        .execute(&pool)
        .await?;
    assert!(
        new.claim(&kernel::queue::ClaimSelector::matching(
            new.task_names(WorkerRole::Embedder)
        ))
        .await?
        .is_none()
    );
    sqlx::query(
        "UPDATE graphile_worker._private_job_queues SET locked_at = now() - interval '5 hours'",
    )
    .execute(&pool)
    .await?;
    let reclaimed = new
        .claim(&kernel::queue::ClaimSelector::matching(
            new.task_names(WorkerRole::Embedder),
        ))
        .await?
        .ok_or("expired claim")?;
    assert_eq!(claimed.id, reclaimed.id);
    let locked = owner(&pool, "serial").await?;
    old.fail(claimed.id, "stale failure").await?;
    old.complete(claimed.id).await?;
    assert_eq!(owner(&pool, "serial").await?, locked);
    new.complete(reclaimed.id).await?;
    assert_eq!(owner(&pool, "serial").await?, None);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_compaction_added_while_one_runs_waits_for_it(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let compact = || kernel::queue::Job {
        name: JobName::Fixed(kernel::queue::FixedJob::SummarizeConversation),
        payload: wire::JsonObject::default(),
        key: Some("compact-one".to_owned()),
        key_mode: kernel::queue::JobKeyMode::Replace,
        priority: 0,
        run_at: None,
        max_attempts: None,
    };
    let left = PostgresQueue::connect(pool.clone()).await?;
    let right = PostgresQueue::connect(pool.clone()).await?;
    let selector = ClaimSelector::matching(left.task_names(WorkerRole::Summarizer));
    left.enqueue(compact()).await?;
    let running = left.claim(&selector).await?.ok_or("first compaction")?;
    let waiting = right.enqueue(compact()).await?;
    assert_ne!(waiting, running.id, "the running job keeps going");
    assert!(right.claim(&selector).await?.is_none());
    left.complete(running.id).await?;
    let next = right.claim(&selector).await?.ok_or("queued compaction")?;
    assert_eq!(next.id, waiting);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_claim_ceiling_leaves_lower_priority_jobs_for_others(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    sqlx::query("SELECT graphile_worker.add_job('embed-target', '{}'::json, priority => 50)")
        .execute(&pool)
        .await?;
    let capped = PostgresQueue::connect(pool.clone())
        .await?
        .claiming_at_most(Some(49));
    let selector = ClaimSelector::matching(capped.task_names(WorkerRole::Embedder));
    assert!(capped.claim(&selector).await?.is_none());
    let open = PostgresQueue::connect(pool.clone()).await?;
    assert!(open.claim(&selector).await?.is_some());
    Ok(())
}
