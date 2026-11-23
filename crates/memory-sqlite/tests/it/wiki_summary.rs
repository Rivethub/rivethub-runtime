use kernel::wiki::builder::ExtractWiki;
use uuid::Uuid;

use crate::common::TestResult;
use crate::common::fresh_store;
use crate::common::seed as seed_error;

#[tokio::test]
async fn a_second_extraction_keeps_the_stable_summary() -> TestResult {
    let fresh = fresh_store()?;
    let jobs = seed_jobs(&fresh.store).await?;
    testkit::maintenance::a_second_extraction_keeps_the_stable_summary(&fresh.store, &jobs).await
}

#[tokio::test]
async fn exhausted_extract_wiki_leaves_the_queue() -> TestResult {
    let fresh = fresh_store()?;
    let jobs = seed_jobs(&fresh.store).await?;
    let summary = jobs.first().ok_or("a live summary")?.summary_id;
    testkit::maintenance::exhausted_extract_wiki_leaves_the_queue(
        &fresh.store,
        &fresh.store,
        summary,
        async || {
            crate::common::execute(
                &fresh.store,
                "UPDATE ros_jobs SET run_at = created_at WHERE task = 'extract-wiki'",
            )
            .await
        },
    )
    .await
}

async fn seed_jobs(store: &memory_sqlite::SqliteStore) -> TestResult<[ExtractWiki; 2]> {
    let first = Uuid::parse_str("0199e2a0-7b3c-7a00-8000-000000000011")?;
    let second = Uuid::parse_str("0199e2a0-7b3c-7a00-8000-000000000012")?;
    let now = "2026-10-28T12:00:00.000Z".to_owned();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_conversations (
                    id, session_key, agent, created_at, updated_at, owner_user_id
                 ) VALUES (
                    '00000000-0000-4000-8000-0000000000c1', 'cli:local', 'rivet', ?1, ?1, 'owner'
                 )",
                [&now],
            )
            .map_err(seed_error)?;
            conn.execute(
                "INSERT INTO ros_summaries (id, conversation_id, content, created_at)
                 VALUES (?1, '00000000-0000-4000-8000-0000000000c1', 'Ships 1.0.', ?2)",
                (first.to_string(), &now),
            )
            .map_err(seed_error)?;
            conn.execute(
                "INSERT INTO ros_summaries (id, conversation_id, content, created_at)
                 VALUES (?1, '00000000-0000-4000-8000-0000000000c1', 'Builds moved.', ?2)",
                (second.to_string(), &now),
            )
            .map_err(seed_error)?;
            Ok(())
        })
        .await?;
    Ok([
        ExtractWiki {
            summary_id: first,
            conversation_id: None,
        },
        ExtractWiki {
            summary_id: second,
            conversation_id: None,
        },
    ])
}
