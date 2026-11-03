use crate::common;

use kernel::flagged::FlaggedJobs as _;
use kernel::queue::ClaimSelector;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue as _;
use memory_postgres::PostgresMemory;
use memory_postgres::PostgresQueue;
use sqlx::PgPool;
use wire::JsonObject;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(serde::Serialize)]
struct Payload<'a> {
    #[serde(rename = "summaryId")]
    summary_id: &'a str,
}

fn job(key: &str, max_attempts: i32) -> Result<Job, serde_json::Error> {
    Ok(Job {
        name: JobName::Fixed(FixedJob::ExtractWiki),
        payload: JsonObject::from_serialize(&Payload { summary_id: key })?,
        key: Some(key.to_owned()),
        key_mode: JobKeyMode::KeepRunAt,
        priority: 10,
        run_at: None,
        max_attempts: Some(max_attempts),
    })
}

async fn fail_once(pool: &PgPool, queue: &PostgresQueue, error: &str) -> TestResult {
    sqlx::query("UPDATE graphile_worker._private_jobs SET run_at = now()")
        .execute(pool)
        .await?;
    let claimed = queue
        .claim(&ClaimSelector::matching(vec![JobName::Fixed(
            FixedJob::ExtractWiki,
        )]))
        .await?
        .ok_or("a job to claim")?;
    queue.fail(claimed.id, error).await?;
    Ok(())
}

async fn queued(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM graphile_worker._private_jobs")
        .fetch_one(pool)
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn failures_count_across_re_adds_and_flag_the_job_out_of_the_queue(
    pool: PgPool,
) -> TestResult {
    common::install_graphile(&pool).await?;
    let queue = PostgresQueue::connect(pool.clone())
        .await?
        .flagging_after(3);
    let store = PostgresMemory::new(pool.clone());
    queue.enqueue(job("bad", 25)?).await?;
    fail_once(&pool, &queue, "slug conflicts with ros-1").await?;
    queue.enqueue(job("bad", 25)?).await?;
    fail_once(&pool, &queue, "slug conflicts with ros-2").await?;
    assert_eq!(queued(&pool).await?, 1, "two failures keep the job queued");
    fail_once(&pool, &queue, "slug conflicts with ros-3").await?;
    assert_eq!(queued(&pool).await?, 0, "the third failure flags it out");
    let flagged = store.flagged().await?;
    let only = flagged.first().ok_or("one flagged job")?;
    assert_eq!(flagged.len(), 1);
    assert_eq!(only.key.as_deref(), Some("bad"));
    assert_eq!(only.failures, 3);
    assert_eq!(only.error, "slug conflicts with ros-3");
    assert!(only.first_failed_at < Some(only.last_failed_at));
    queue.enqueue(job("good", 1)?).await?;
    fail_once(&pool, &queue, "slug conflicts with ros-4").await?;
    let groups = kernel::flagged::group(store.flagged().await?);
    assert_eq!(groups.len(), 1, "one cause shows once");
    assert_eq!(groups.first().map(|group| group.count), Some(2));
    store.retry(only.id).await?;
    assert_eq!(queued(&pool).await?, 1, "retry puts the job back");
    let claimed = queue
        .claim(&ClaimSelector::matching(vec![JobName::Fixed(
            FixedJob::ExtractWiki,
        )]))
        .await?
        .ok_or("the retried job")?;
    queue.complete(claimed.id).await?;
    let rest = store.flagged().await?;
    assert_eq!(rest.len(), 1);
    let good = rest.first().ok_or("the other flagged job")?;
    store.dismiss(good.id, chrono::Utc::now()).await?;
    assert_eq!(store.flagged().await?, Vec::new());
    assert!(store.dismiss(good.id, chrono::Utc::now()).await.is_err());
    let ledger: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_job_failures")
        .fetch_one(&pool)
        .await?;
    assert_eq!(ledger, 1, "only the dismissed key keeps its count");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_sweep_flags_jobs_that_already_ran_out_of_attempts(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let queue = PostgresQueue::connect(pool.clone()).await?;
    queue.enqueue(job("dead", 2)?).await?;
    queue.enqueue(job("live", 2)?).await?;
    sqlx::query(
        "UPDATE graphile_worker._private_jobs SET attempts = max_attempts, last_error = 'boom'
          WHERE key = 'dead'",
    )
    .execute(&pool)
    .await?;
    assert_eq!(queue.flag_stuck().await?, 1);
    assert_eq!(queued(&pool).await?, 1);
    let flagged = PostgresMemory::new(pool.clone()).flagged().await?;
    assert_eq!(
        flagged
            .iter()
            .map(|job| (job.key.as_deref(), job.failures, job.error.as_str()))
            .collect::<Vec<_>>(),
        vec![(Some("dead"), 2, "boom")]
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_flagged_job_is_named_by_the_conversation_it_worked_on(pool: PgPool) -> TestResult {
    let conversation: sqlx::types::Uuid = sqlx::query_scalar(
        "INSERT INTO rh_conversations (session_key, agent, title, owner_user_id)
         VALUES ('claude-code:one', 'claude', 'Fix the flaky gate', 'owner') RETURNING id",
    )
    .fetch_one(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO rh_flagged_jobs (task, job_key, payload, priority, max_attempts, failures,
                                      last_error, first_failed_at, last_failed_at, flagged_at)
         VALUES ('compact-conversation', 'named', jsonb_build_object('conversationId', $1::text),
                 0, 5, 5, 'boom', now(), now(), now()),
                ('synthesize-tool-call', 'torn', '{\"messageId\": \"not-a-uuid\"}',
                 0, 5, 5, 'boom', now(), now() - interval '1 minute', now())",
    )
    .bind(conversation.to_string())
    .execute(&pool)
    .await?;
    let named: Vec<(Option<String>, Option<String>)> = PostgresMemory::new(pool)
        .flagged()
        .await?
        .into_iter()
        .map(|job| (job.key, job.subject))
        .collect();
    assert_eq!(
        named,
        [
            (
                Some("named".to_owned()),
                Some("Fix the flaky gate".to_owned())
            ),
            (Some("torn".to_owned()), None)
        ]
    );
    Ok(())
}
