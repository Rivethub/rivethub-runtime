use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SCANNED: &str = "SELECT conversation_id, owner_user_id, coalesce(tool_name, '') AS tool_name,
        count(*)::bigint AS total,
        count(*) FILTER (WHERE role = 'tool' OR tool_name IS NOT NULL)::bigint AS tool_calls,
        count(embedding)::bigint AS embedded,
        count(*) FILTER (WHERE embedding IS NULL AND embed_status IS DISTINCT FROM 'unembeddable'
            AND embed_status IS DISTINCT FROM 'failed' AND (content <> '' OR tool_result <> ''))::bigint AS queued,
        count(*) FILTER (WHERE embed_status = 'failed')::bigint AS failed
   FROM rh_messages GROUP BY 1, 2, 3 ORDER BY 1, 2, 3";

const COUNTED: &str =
    "SELECT conversation_id, owner_user_id, tool_name, total, tool_calls, embedded, queued, failed
   FROM rh_message_counts WHERE total <> 0 ORDER BY 1, 2, 3";

type Row = (Uuid, String, String, i64, i64, i64, i64, i64);

async fn tallies(pool: &PgPool, sql: &'static str) -> Result<Vec<Row>, sqlx::Error> {
    sqlx::query_as(sql).fetch_all(pool).await
}

async fn conversation(pool: &PgPool, session: &str, owner: &str) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO rh_conversations (session_key, agent, owner_user_id) VALUES ($1, 'rivet', $2) RETURNING id",
    )
    .bind(session)
    .bind(owner)
    .fetch_one(pool)
    .await
}

async fn messages(pool: &PgPool, conversation: Uuid, owner: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_messages (conversation_id, agent, channel, role, content, tool_name, tool_result, owner_user_id)
         VALUES ($1, 'rivet', 'cli', 'user', 'plan the kitchen shelves', NULL, NULL, $2),
                ($1, 'rivet', 'cli', 'assistant', 'measured the wall twice', NULL, NULL, $2),
                ($1, 'rivet', 'cli', 'tool', '', 'Bash', 'ls -la', $2),
                ($1, 'rivet', 'cli', 'tool', '', 'Bash', '', $2)",
    )
    .bind(conversation)
    .bind(owner)
    .execute(pool)
    .await
    .map(drop)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn message_counts_follow_every_write(pool: PgPool) -> TestResult {
    crate::common::install_graphile(&pool).await?;
    let kitchen = conversation(&pool, "claude-code:kitchen", "phil").await?;
    let garden = conversation(&pool, "claude-code:garden", "coco").await?;
    messages(&pool, kitchen, "phil").await?;
    messages(&pool, garden, "coco").await?;
    sqlx::query(
        "UPDATE rh_messages SET embedding = array_fill(0.5::real, ARRAY[1024])::halfvec
          WHERE role = 'user'",
    )
    .execute(&pool)
    .await?;
    sqlx::query("UPDATE rh_messages SET embed_status = 'failed' WHERE role = 'assistant' AND conversation_id = $1")
        .bind(garden)
        .execute(&pool)
        .await?;
    sqlx::query("UPDATE rh_messages SET owner_user_id = 'phil' WHERE conversation_id = $1 AND role = 'tool'")
        .bind(garden)
        .execute(&pool)
        .await?;
    sqlx::query("DELETE FROM rh_messages WHERE conversation_id = $1 AND role = 'assistant'")
        .bind(kitchen)
        .execute(&pool)
        .await?;
    assert_eq!(
        tallies(&pool, COUNTED).await?,
        tallies(&pool, SCANNED).await?
    );

    let memory = PostgresMemory::new(pool.clone());
    let stats = kernel::tenancy::Scope::Maintenance
        .run(memory.memory_stats())
        .await?;
    assert_eq!(
        (stats.messages, stats.tool_calls, stats.embedded_messages),
        (7, 4, 2)
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn existing_conversations_fill_in_batches(pool: PgPool) -> TestResult {
    crate::common::install_graphile(&pool).await?;
    let kitchen = conversation(&pool, "claude-code:kitchen", "phil").await?;
    let garden = conversation(&pool, "claude-code:garden", "phil").await?;
    sqlx::query("INSERT INTO rh_message_counts_pending (conversation_id) VALUES ($1), ($2)")
        .bind(kitchen)
        .bind(garden)
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO rh_backfills (name) VALUES ('message-counts')")
        .execute(&pool)
        .await?;
    messages(&pool, kitchen, "phil").await?;
    messages(&pool, garden, "phil").await?;
    assert!(
        tallies(&pool, COUNTED).await?.is_empty(),
        "a conversation waiting for its backfill is not counted twice"
    );

    let left: i64 = sqlx::query_scalar("SELECT rh_backfill_message_counts(1)")
        .fetch_one(&pool)
        .await?;
    assert_eq!(left, 1);
    messages(&pool, kitchen, "phil").await?;
    let left: i64 = sqlx::query_scalar("SELECT rh_backfill_message_counts(1)")
        .fetch_one(&pool)
        .await?;
    assert_eq!(left, 0);
    assert_eq!(
        tallies(&pool, COUNTED).await?,
        tallies(&pool, SCANNED).await?
    );
    let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_backfills")
        .fetch_one(&pool)
        .await?;
    assert_eq!(waiting, 0, "the finished backfill clears its marker");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn chunks_of_said_messages_are_marked(pool: PgPool) -> TestResult {
    crate::common::install_graphile(&pool).await?;
    let kitchen = conversation(&pool, "claude-code:kitchen", "phil").await?;
    messages(&pool, kitchen, "phil").await?;
    sqlx::query(
        "INSERT INTO rh_message_chunks (message_id, idx, char_start, char_end, content, owner_user_id)
         SELECT id, 0, 0, 10, content, owner_user_id FROM rh_messages",
    )
    .execute(&pool)
    .await?;
    let said: Vec<(String, bool)> = sqlx::query_as(
        "SELECT m.role, c.said FROM rh_message_chunks c JOIN rh_messages m ON m.id = c.message_id ORDER BY 1, 2",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        said,
        vec![
            ("assistant".to_owned(), true),
            ("tool".to_owned(), false),
            ("tool".to_owned(), false),
            ("user".to_owned(), true)
        ]
    );

    sqlx::query("UPDATE rh_message_chunks SET said = false")
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO rh_backfills (name) VALUES ('chunk-said')")
        .execute(&pool)
        .await?;
    let mut after: Option<Uuid> = None;
    let mut calls = 0;
    loop {
        let last: Option<Uuid> = sqlx::query_scalar("SELECT rh_backfill_chunk_said($1, 3)")
            .bind(after)
            .fetch_one(&pool)
            .await?;
        calls += 1;
        let Some(last) = last else { break };
        after = Some(last);
    }
    assert_eq!(calls, 3, "two batches of three chunks, then the end");
    let marked: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_message_chunks WHERE said")
        .fetch_one(&pool)
        .await?;
    assert_eq!(marked, 2);
    let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_backfills")
        .fetch_one(&pool)
        .await?;
    assert_eq!(waiting, 0);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn deleting_rows_loaded_without_triggers_recounts_instead_of_going_negative(
    pool: PgPool,
) -> TestResult {
    crate::common::install_graphile(&pool).await?;
    let kitchen = conversation(&pool, "claude-code:kitchen", "phil").await?;
    messages(&pool, kitchen, "phil").await?;
    sqlx::query("ALTER TABLE rh_messages DISABLE TRIGGER rh_message_counts_inserted")
        .execute(&pool)
        .await?;
    messages(&pool, kitchen, "phil").await?;
    sqlx::query("ALTER TABLE rh_messages ENABLE TRIGGER rh_message_counts_inserted")
        .execute(&pool)
        .await?;
    sqlx::query(
        "DELETE FROM rh_messages WHERE id IN (
             SELECT id FROM rh_messages WHERE conversation_id = $1 ORDER BY created_at DESC, id LIMIT 6)",
    )
    .bind(kitchen)
    .execute(&pool)
    .await?;
    let negative: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM rh_message_counts WHERE total < 0 OR tool_calls < 0 OR queued < 0",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(negative, 0, "no counter goes below zero");
    assert_eq!(
        tallies(&pool, COUNTED).await?,
        tallies(&pool, SCANNED).await?,
        "the counter is recounted from the rows that remain"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_bad_stored_counter_degrades_one_field_and_the_answers_still_come(
    pool: PgPool,
) -> TestResult {
    use kernel::facts::FactStore as _;
    use kernel::memory::MemoryStore as _;
    crate::common::install_graphile(&pool).await?;
    let session = "claude-code:00000000-0000-4000-8000-0000000000f1";
    let kitchen = conversation(&pool, session, "phil").await?;
    messages(&pool, kitchen, "phil").await?;
    sqlx::query("UPDATE rh_message_counts SET total = -3 WHERE conversation_id = $1")
        .bind(kitchen)
        .execute(&pool)
        .await?;
    let memory = PostgresMemory::new(pool.clone());
    let found = kernel::tenancy::Scope::Maintenance
        .run(memory.fact_sessions(&[session.parse()?]))
        .await?;
    assert_eq!(found.len(), 1, "the session is still described");
    assert!(
        found.iter().all(|session| session.messages == 0),
        "the bad count reads as zero"
    );
    kernel::tenancy::Scope::Maintenance
        .run(memory.stats())
        .await?;
    Ok(())
}
