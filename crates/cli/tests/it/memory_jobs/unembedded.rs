use jiff::Timestamp;
use kernel::memory::jobs::UnembeddedStore;
use kernel::memory::{EmbedTable, EmbedTarget};
use kernel::queue::{FixedJob, Job, JobKeyMode, JobName, JobQueue};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;
use workers::embedding_sweep::handle_enqueue_unembedded;

use super::support::{RecordingQueue, TestResult, message_id, seed};

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn unembedded_heals_only_transient_poison_and_enqueues_typed_jobs(
    pool: PgPool,
) -> TestResult {
    let store = seed(&pool).await;
    let queue = RecordingQueue::new(&pool).await;
    sqlx::query("UPDATE rh_messages SET tool_result = repeat('r', 21), embed_status = 'failed', embed_error = 'Embedding returned null', embed_failures = 8").execute(&pool).await?;
    for (id, status, error, content) in [
        (
            3,
            Some("failed"),
            Some("Embedding returned null (permanent)"),
            "long enough for embedding",
        ),
        (
            4,
            Some("failed"),
            Some("other failure"),
            "long enough for embedding",
        ),
        (5, Some("failed"), Some("Embedding returned null"), "short"),
        (6, None, None, "long enough for embedding"),
        (7, Some("unembeddable"), None, "long enough for embedding"),
    ] {
        sqlx::query("INSERT INTO rh_summaries (id, content, embed_status, embed_error, embed_failures, owner_user_id) VALUES ($1,$2,$3,$4,3, 'owner')")
            .bind(uuid::Uuid::from_u128(id)).bind(content).bind(status).bind(error).execute(&pool).await?;
    }
    sqlx::query("DELETE FROM graphile_worker._private_jobs")
        .execute(&pool)
        .await?;
    assert_eq!(
        handle_enqueue_unembedded(&store, &queue, 100, 5, &CancellationToken::new()).await?,
        2
    );
    let expected: Vec<Job> = [
        (EmbedTable::Messages, message_id()),
        (EmbedTable::Summaries, uuid::Uuid::from_u128(6)),
    ]
    .into_iter()
    .map(|(table, id)| Job {
        name: JobName::Fixed(FixedJob::EmbedTarget),
        payload: JsonObject::from_serialize(&EmbedPayload {
            target_table: table.to_string(),
            target_id: id,
        })
        .expect("payload"),
        key: Some(format!("embed-{table}-{id}")),
        key_mode: JobKeyMode::KeepRunAt,
        priority: 0,
        run_at: None,
        max_attempts: Some(5),
    })
    .collect();
    assert_eq!(*queue.jobs.lock(), expected);
    let state: (Option<String>, Option<String>, i32) = sqlx::query_as(
        "SELECT embed_status, embed_error, embed_failures FROM rh_messages WHERE id = $1",
    )
    .bind(message_id())
    .fetch_one(&pool)
    .await?;
    assert_eq!(state, (None, None, 0));
    let failed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rh_summaries WHERE embed_status = 'failed'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(failed, 3);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn unembedded_preserves_scheduled_run_at_and_selects_newest_per_table(
    pool: PgPool,
) -> TestResult {
    let store = seed(&pool).await;
    let queue = RecordingQueue::new(&pool).await;
    for (id, date) in [(3, "2026-10-01"), (4, "2026-10-02")] {
        sqlx::query("INSERT INTO rh_summaries (id, content, created_at, owner_user_id) VALUES ($1, repeat('x',21), $2::text::timestamptz, 'owner')").bind(uuid::Uuid::from_u128(id)).bind(date).execute(&pool).await?;
    }
    sqlx::query("DELETE FROM graphile_worker._private_jobs")
        .execute(&pool)
        .await?;
    let target = EmbedTarget {
        table: EmbedTable::Summaries,
        id: uuid::Uuid::from_u128(4).to_string(),
    };
    let future: Timestamp = "2099-01-01T00:00:00Z".parse()?;
    let mut job = workers::embedding_sweep::embed_sweep_job(&target, 5)?;
    job.run_at = Some(future);
    queue.enqueue(job).await?;
    queue.jobs.lock().clear();
    assert_eq!(
        handle_enqueue_unembedded(&store, &queue, 1, 7, &CancellationToken::new()).await?,
        1
    );
    let run_at: memory_postgres::time::PgTimestamp =
        sqlx::query_scalar("SELECT run_at FROM graphile_worker._private_jobs WHERE key = $1")
            .bind(format!("embed-ros_summaries-{}", target.id))
            .fetch_one(&pool)
            .await?;
    assert_eq!(run_at.to_jiff(), future);
    assert_eq!(
        queue.jobs.lock().first().expect("job").max_attempts,
        Some(7)
    );
    assert_eq!(
        store.unembedded(EmbedTable::Summaries, 1).await?,
        vec![target]
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn unembedded_tolerates_missing_optional_tables_and_refuses_missing_required_tables(
    pool: PgPool,
) -> TestResult {
    let store = seed(&pool).await;
    let queue = RecordingQueue::new(&pool).await;
    sqlx::raw_sql("DROP TABLE rh_wiki_topics CASCADE; DROP TABLE rh_message_chunks CASCADE;")
        .execute(&pool)
        .await?;
    assert_eq!(
        handle_enqueue_unembedded(&store, &queue, 100, 5, &CancellationToken::new()).await?,
        0
    );
    sqlx::query("DROP TABLE rh_messages CASCADE")
        .execute(&pool)
        .await?;
    assert!(
        handle_enqueue_unembedded(&store, &queue, 100, 5, &CancellationToken::new())
            .await
            .is_err()
    );
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EmbedPayload {
    target_table: String,
    target_id: uuid::Uuid,
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn unembedded_heals_wiki_but_never_heals_chunks(pool: PgPool) -> TestResult {
    let store = seed(&pool).await;
    let queue = RecordingQueue::new(&pool).await;
    sqlx::query("INSERT INTO rh_wiki_topics (slug,title,search_text,embed_status,embed_error,embed_failures) VALUES ('fixture-topic','Fixture topic',repeat('w',21),'failed','Embedding returned null',2)")
        .execute(&pool).await?;
    for (index, content, status) in [
        (0, "chunk", None),
        (1, "  ", None),
        (2, "failed chunk", Some("failed")),
    ] {
        sqlx::query("INSERT INTO rh_message_chunks (id,message_id,idx,char_start,char_end,content,embed_status,embed_error,embed_failures, owner_user_id) VALUES ($1,$2,$3,0,20,$4,$5,'Embedding returned null',2, 'owner')")
            .bind(uuid::Uuid::from_u128(u128::try_from(index + 10)?)).bind(message_id()).bind(index).bind(content).bind(status).execute(&pool).await?;
    }
    sqlx::query("DELETE FROM graphile_worker._private_jobs")
        .execute(&pool)
        .await?;
    assert_eq!(
        handle_enqueue_unembedded(&store, &queue, 100, 5, &CancellationToken::new()).await?,
        2
    );
    let actual: Vec<_> = queue
        .jobs
        .lock()
        .iter()
        .map(|job| job.key.clone())
        .collect();
    assert_eq!(
        actual,
        vec![
            Some("embed-ros_wiki_topics-fixture-topic".to_owned()),
            Some(format!(
                "embed-ros_message_chunks-{}",
                uuid::Uuid::from_u128(10)
            ))
        ]
    );
    let failures: i32 =
        sqlx::query_scalar("SELECT embed_failures FROM rh_message_chunks WHERE idx = 2")
            .fetch_one(&pool)
            .await?;
    assert_eq!(failures, 2);
    let wiki: (Option<String>, Option<String>, i32) =
        sqlx::query_as("SELECT embed_status, embed_error, embed_failures FROM rh_wiki_topics")
            .fetch_one(&pool)
            .await?;
    assert_eq!(wiki, (None, None, 0));
    Ok(())
}
