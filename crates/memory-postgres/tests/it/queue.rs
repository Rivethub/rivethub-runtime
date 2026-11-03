use crate::common;

use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue as _;
use kernel::queue::WorkerRole;
use memory_postgres::PostgresMemory;
use memory_postgres::PostgresQueue;
use sqlx::PgPool;
use wire::JsonObject;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn object<T: serde::Serialize>(payload: &T) -> JsonObject {
    serde_json::from_value(serde_json::to_value(payload).unwrap_or_default()).unwrap_or_default()
}

#[derive(serde::Serialize)]
struct EmbedTargetPayload<'a> {
    #[serde(rename = "targetTable")]
    target_table: &'a str,
    #[serde(rename = "targetId")]
    target_id: &'a str,
}

#[derive(serde::Serialize)]
struct SummarizeConversationPayload<'a> {
    #[serde(rename = "conversationId")]
    conversation_id: &'a str,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CompactPayload<'a> {
    conversation_id: &'a str,
    trigger_type: &'a str,
}

#[derive(serde::Serialize)]
struct ExtractWikiPayload<'a> {
    #[serde(rename = "summaryId")]
    summary_id: &'a str,
}

async fn queue(pool: &PgPool) -> Result<PostgresQueue, Box<dyn std::error::Error>> {
    Ok(PostgresQueue::connect(pool.clone()).await?)
}

fn store(pool: &PgPool) -> PostgresMemory {
    PostgresMemory::new(pool.clone())
}

async fn stored_jobs(
    pool: &PgPool,
) -> Result<Vec<(String, Option<String>, i64, i64, i64)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT t.identifier, j.key, j.priority::bigint AS priority,
                j.max_attempts::bigint AS max_attempts, j.attempts::bigint AS attempts
           FROM graphile_worker._private_jobs j
           JOIN graphile_worker._private_tasks t ON t.id = j.task_id
          ORDER BY j.id",
    )
    .fetch_all(pool)
    .await
}

fn embed_job() -> Job {
    Job {
        name: JobName::Fixed(FixedJob::EmbedTarget),
        payload: object(&EmbedTargetPayload {
            target_table: "ros_messages",
            target_id: "abc",
        }),
        key: Some("embed-ros_messages-abc".to_string()),
        key_mode: JobKeyMode::Replace,
        priority: 0,
        run_at: None,
        max_attempts: None,
    }
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn enqueue_claims_and_completes_through_the_public_api(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let jobs = queue(&pool).await?;

    jobs.enqueue(embed_job()).await?;
    let rows = stored_jobs(&pool).await?;
    assert_eq!(
        rows.first(),
        Some(&(
            "embed-target".to_string(),
            Some("embed-ros_messages-abc".to_string()),
            0,
            5,
            0
        ))
    );

    let claimed = jobs
        .claim(&kernel::queue::ClaimSelector::matching(
            jobs.task_names(WorkerRole::Embedder),
        ))
        .await?;
    let claimed = claimed.ok_or("the only job is claimable")?;
    assert_eq!(claimed.name, JobName::Fixed(FixedJob::EmbedTarget));
    assert_eq!(claimed.attempt, 1);
    assert!(
        claimed
            .payload
            .get("targetId")
            .and_then(|value| value.as_str())
            .is_some_and(|target| target == "abc")
    );
    assert!(
        jobs.claim(&kernel::queue::ClaimSelector::matching(
            jobs.task_names(WorkerRole::Embedder)
        ))
        .await?
        .is_none(),
        "the claimed job is locked"
    );

    jobs.fail(claimed.id, "boom").await?;
    assert!(
        jobs.claim(&kernel::queue::ClaimSelector::matching(
            jobs.task_names(WorkerRole::Embedder)
        ))
        .await?
        .is_none(),
        "a failed job waits out graphile's exponential backoff"
    );
    sqlx::query("UPDATE graphile_worker._private_jobs SET run_at = now()")
        .execute(&pool)
        .await?;
    let retried = jobs
        .claim(&kernel::queue::ClaimSelector::matching(
            jobs.task_names(WorkerRole::Embedder),
        ))
        .await?
        .ok_or("the failed job is retried after its backoff")?;
    assert_eq!(retried.id, claimed.id);
    assert_eq!(retried.attempt, 2);

    jobs.complete(retried.id).await?;
    assert!(
        jobs.claim(&kernel::queue::ClaimSelector::matching(
            jobs.task_names(WorkerRole::Embedder)
        ))
        .await?
        .is_none(),
        "completion removes the job"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_postponed_job_waits_its_turn_unless_claimed_by_id(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let jobs = queue(&pool).await?;
    let any = kernel::queue::ClaimSelector::matching(jobs.task_names(WorkerRole::Embedder));
    jobs.enqueue(embed_job()).await?;
    let claimed = jobs.claim(&any).await?.ok_or("the only job is claimable")?;
    jobs.postpone(claimed.id, std::time::Duration::from_secs(60))
        .await?;
    assert!(jobs.claim(&any).await?.is_none(), "a postponed job waits");
    let by_id = kernel::queue::ClaimSelector {
        only_id: Some(claimed.id),
        ..any
    };
    let joined = jobs
        .claim(&by_id)
        .await?
        .ok_or("a coalescing lead claims a postponed job by id")?;
    assert_eq!(joined.attempt, 1, "postponing gave the attempt back");
    jobs.complete(joined.id).await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn arming_keeps_a_waiting_job_in_line_and_only_raises_it(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let jobs = queue(&pool).await?;
    let arm = |priority: i16, trigger: &str| Job {
        name: JobName::Fixed(FixedJob::SummarizeConversation),
        payload: object(&CompactPayload {
            conversation_id: "c1",
            trigger_type: trigger,
        }),
        key: Some("compact-c1".to_owned()),
        key_mode: JobKeyMode::Arm,
        priority,
        run_at: Some(chrono::Utc::now()),
        max_attempts: Some(3),
    };
    let state = || {
        sqlx::query_as::<_, (i16, bool, Option<String>)>(
            "SELECT priority, run_at < now() - interval '6 days', payload->>'triggerType'
               FROM graphile_worker._private_jobs WHERE key = 'compact-c1'",
        )
        .fetch_one(&pool)
    };
    jobs.enqueue(arm(10, "session_stale")).await?;
    sqlx::query("UPDATE graphile_worker._private_jobs SET run_at = now() - interval '7 days'")
        .execute(&pool)
        .await?;
    jobs.enqueue(arm(10, "session_stale")).await?;
    assert_eq!(state().await?, (10, true, Some("session_stale".to_owned())));
    jobs.enqueue(arm(0, "session_idle")).await?;
    assert_eq!(state().await?, (0, true, Some("session_idle".to_owned())));
    jobs.enqueue(arm(10, "session_stale")).await?;
    assert_eq!(state().await?, (0, true, Some("session_idle".to_owned())));
    let running = jobs
        .claim(&kernel::queue::ClaimSelector::matching(
            jobs.task_names(WorkerRole::Summarizer),
        ))
        .await?
        .ok_or("the armed job runs first")?;
    jobs.enqueue(arm(0, "session_idle")).await?;
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM graphile_worker._private_jobs")
        .fetch_one(&pool)
        .await?;
    assert_eq!(queued, 2, "arming a running job queues one behind it");
    jobs.complete(running.id).await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn job_keys_replace_preserve_and_revive(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let jobs = queue(&pool).await?;
    let summarize = |payload: JsonObject| Job {
        name: JobName::Fixed(FixedJob::SummarizeConversation),
        payload,
        key: Some("conv-key".to_string()),
        key_mode: JobKeyMode::Replace,
        priority: 0,
        run_at: None,
        max_attempts: None,
    };

    jobs.enqueue(summarize(object(&SummarizeConversationPayload {
        conversation_id: "one",
    })))
    .await?;
    jobs.enqueue(summarize(object(&SummarizeConversationPayload {
        conversation_id: "two",
    })))
    .await?;
    let rows = stored_jobs(&pool).await?;
    assert_eq!(rows.len(), 1, "replace must reuse the keyed row");

    sqlx::query("UPDATE graphile_worker._private_jobs SET attempts = max_attempts")
        .execute(&pool)
        .await?;
    jobs.enqueue(summarize(object(&SummarizeConversationPayload {
        conversation_id: "three",
    })))
    .await?;
    let rows = stored_jobs(&pool).await?;
    assert_eq!(
        rows.len(),
        2,
        "a dead keyed row becomes a corpse and a fresh row"
    );
    let corpse = rows.first().cloned().ok_or("the corpse row")?;
    let fresh = rows.get(1).cloned().ok_or("the fresh row")?;
    assert_eq!(corpse.1, None, "the corpse loses its key");
    assert_eq!(fresh.1.as_deref(), Some("conv-key"));
    assert_eq!(fresh.4, 0, "the fresh row restarts attempts");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn role_scoping_keeps_the_task_lists_apart(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let jobs = queue(&pool).await?;
    assert!(
        jobs.claim(&kernel::queue::ClaimSelector::matching(
            jobs.task_names(WorkerRole::Summarizer)
        ))
        .await?
        .is_none(),
        "an empty queue claims nothing"
    );

    jobs.enqueue(embed_job()).await?;
    assert!(
        jobs.claim(&kernel::queue::ClaimSelector::matching(
            jobs.task_names(WorkerRole::Summarizer)
        ))
        .await?
        .is_none(),
        "the summarizer role must not claim embed jobs"
    );
    let embedder_claim = jobs
        .claim(&kernel::queue::ClaimSelector::matching(
            jobs.task_names(WorkerRole::Embedder),
        ))
        .await?;
    let claimed = embedder_claim.ok_or("the embedder claims its own job")?;
    jobs.complete(claimed.id).await?;

    jobs.enqueue(Job {
        name: JobName::Fixed(FixedJob::ExtractWiki),
        payload: object(&ExtractWikiPayload { summary_id: "s1" }),
        key: Some("wiki-ext-s1".to_string()),
        key_mode: JobKeyMode::Replace,
        priority: 5,
        run_at: None,
        max_attempts: None,
    })
    .await?;
    sqlx::query(
        "UPDATE graphile_worker._private_jobs
            SET attempts = max_attempts, updated_at = now() - interval '2 days'",
    )
    .execute(&pool)
    .await?;

    assert_eq!(
        jobs.reschedule_dead(&JobName::Fixed(FixedJob::ExtractWiki), 200, 5)
            .await?,
        1
    );
    let revived = stored_jobs(&pool).await?;
    assert_eq!(
        revived.first().map(|row| row.4),
        Some(0),
        "revive resets attempts"
    );

    sqlx::query(
        "UPDATE graphile_worker._private_jobs
            SET attempts = max_attempts, key = null,
                updated_at = now() - interval '8 days'",
    )
    .execute(&pool)
    .await?;
    let reaped = jobs
        .reap_dead_corpses(
            &[
                JobName::Fixed(FixedJob::ExtractWiki),
                JobName::Fixed(FixedJob::SummarizeConversation),
                JobName::Fixed(FixedJob::EmbedTarget),
            ],
            200,
        )
        .await?;
    assert_eq!(reaped, 1, "the keyless corpse is reaped");
    assert_eq!(stored_jobs(&pool).await?, Vec::new());
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn queue_health_reports_buckets(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let jobs = queue(&pool).await?;
    jobs.enqueue(Job {
        name: JobName::Fixed(FixedJob::ExtractWiki),
        payload: object(&ExtractWikiPayload { summary_id: "s1" }),
        key: None,
        key_mode: JobKeyMode::Replace,
        priority: 5,
        run_at: None,
        max_attempts: None,
    })
    .await?;
    sqlx::query("UPDATE graphile_worker._private_jobs SET attempts = max_attempts")
        .execute(&pool)
        .await?;

    let health = jobs
        .queue_health(&JobName::Fixed(FixedJob::ExtractWiki))
        .await?;
    let health = health.ok_or("the health row exists")?;
    assert_eq!(health.task, JobName::Fixed(FixedJob::ExtractWiki));
    assert_eq!(health.dead, 1);
    assert_eq!(health.pending, 0);

    assert!(
        jobs.queue_health(&JobName::Fixed(FixedJob::SummarizeConversation))
            .await?
            .is_none(),
        "a task without jobs reports no bucket"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_migrations_create_the_queue(pool: PgPool) -> TestResult {
    let jobs = queue(&pool).await?;
    jobs.enqueue(embed_job()).await?;
    let claimed = jobs
        .claim(&kernel::queue::ClaimSelector::matching(
            jobs.task_names(WorkerRole::Embedder),
        ))
        .await?;
    assert_eq!(
        claimed.map(|job| job.name),
        Some(JobName::Fixed(FixedJob::EmbedTarget))
    );
    assert!(store(&pool).queue_available().await?);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_store_reports_a_missing_queue_as_unavailable(pool: PgPool) -> TestResult {
    sqlx::query("DROP SCHEMA graphile_worker CASCADE")
        .execute(&pool)
        .await?;
    assert!(matches!(
        PostgresQueue::connect(pool.clone()).await,
        Err(memory_postgres::Error::QueueAbsent)
    ));
    let memory = store(&pool);
    assert!(!memory.queue_available().await?);
    Ok(())
}

#[test]
fn crontab_and_task_lists_follow_the_roles() {
    let embedder = PostgresQueue::crontab(WorkerRole::Embedder);
    assert_eq!(embedder.len(), 2);
    assert_eq!(
        embedder.first().map(|entry| entry.schedule.as_str()),
        Some("*/10 * * * *")
    );
    assert_eq!(
        embedder.get(1).map(|entry| entry.identifier.as_str()),
        Some("unchunked-sweep")
    );
    let summarizer = PostgresQueue::crontab(WorkerRole::Summarizer);
    assert_eq!(summarizer.len(), 5);
    assert_eq!(
        PostgresQueue::crontab(WorkerRole::Categorizer)
            .iter()
            .map(|entry| (entry.identifier.as_str(), entry.schedule.as_str()))
            .collect::<Vec<_>>(),
        [("tidy-sweep", "0 * * * *")]
    );

    assert_eq!(
        PostgresQueue::task_list(WorkerRole::Embedder),
        &[
            JobName::Fixed(FixedJob::EmbedTarget),
            JobName::Fixed(FixedJob::EnqueueUnembedded),
            JobName::Fixed(FixedJob::EnqueueUnchunked)
        ]
    );
    assert!(
        PostgresQueue::task_list(WorkerRole::Summarizer)
            .contains(&JobName::Fixed(FixedJob::SummarizeConversation))
    );
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_pending_sweep_is_queued_once(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    testkit::sweeps::a_pending_sweep_is_queued_once(&queue(&pool).await?).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_sweeps_run_before_an_older_backlog(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    testkit::sweeps::the_sweeps_run_before_an_older_backlog(&queue(&pool).await?).await
}
