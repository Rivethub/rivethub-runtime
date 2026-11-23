use kernel::owner::Owner;
use memory_sqlite::{SqliteError, SqliteStore};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::super::{Direction, TransferReport, pieces::Limits, run_with_limits, verify};
use super::{TestResult, store};

const LIMITS: Limits = Limits {
    rows: 2,
    bytes: 2048,
};

async fn copy(
    pool: &PgPool,
    local: &SqliteStore,
    direction: Direction,
) -> Result<TransferReport, Box<dyn std::error::Error>> {
    let owner = Owner::new("alice".into())?;
    Ok(run_with_limits(
        pool,
        local,
        [&owner, &owner],
        direction,
        (false, None, LIMITS),
        &CancellationToken::new(),
    )
    .await?)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn bounded_transfer_preserves_jobs_counts_and_replay(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let source = store(&directory, "source.sqlite")?;
    let restored = store(&directory, "restored.sqlite")?;
    seed_conversations(&source).await?;
    copy(&pool, &source, Direction::ToPostgres).await?;
    audit_inserts(&pool).await?;
    seed_messages(&source).await?;

    let first = copy(&pool, &source, Direction::ToPostgres).await?;
    assert_eq!(first.added.get("rh_messages"), Some(&7));
    assert_insert_pieces(&pool).await?;
    assert_counts(&pool).await?;
    let second = copy(&pool, &source, Direction::ToPostgres).await?;
    assert_eq!(second.added.values().sum::<u64>(), 0);
    assert_embed_jobs(&pool).await?;

    let first = copy(&pool, &restored, Direction::ToSqlite).await?;
    assert_eq!(first.added.get("rh_messages"), Some(&7));
    verify::assert_same(
        &verify::sqlite(&pool, &source).await?,
        &verify::sqlite(&pool, &restored).await?,
    );
    let second = copy(&pool, &restored, Direction::ToSqlite).await?;
    assert_eq!(second.added.values().sum::<u64>(), 0);
    Ok(())
}

async fn seed_conversations(source: &SqliteStore) -> TestResult {
    source.with_conn(|conn| conn.execute_batch(
        "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at, owner_user_id)
         VALUES ('00000000-0000-0000-0000-000000000001', 'pending', 'codex', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'alice'),
                ('00000000-0000-0000-0000-000000000002', 'live', 'codex', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'alice')",
    ).map_err(SqliteError::Pragma)).await?;
    Ok(())
}

async fn seed_messages(source: &SqliteStore) -> TestResult {
    source.with_conn(|conn| conn.execute_batch(
        "WITH RECURSIVE messages(n) AS (VALUES (1) UNION ALL SELECT n + 1 FROM messages WHERE n < 7)
         INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, metadata, created_at, owner_user_id)
         SELECT printf('00000000-0000-0000-0000-%012d', 10 + n),
                printf('00000000-0000-0000-0000-%012d', 1 + n % 2),
                'codex', 'cli', 'user', CASE n WHEN 1 THEN '0123456789'
                   WHEN 4 THEN printf('%04096d', 0)
                   WHEN 2 THEN printf('%01200d', 0) WHEN 3 THEN printf('%01200d', 1)
                   ELSE 'small' END,
                '{}', '2026-01-01T00:00:01Z', 'alice'
           FROM messages",
    ).map_err(SqliteError::Pragma)).await?;
    Ok(())
}

async fn audit_inserts(pool: &PgPool) -> TestResult {
    sqlx::raw_sql(
        "INSERT INTO rh_message_counts_pending (conversation_id)
         SELECT id FROM rh_conversations WHERE session_key = 'pending';
         CREATE TABLE transfer_insert_sizes (rows bigint NOT NULL, content_bytes bigint NOT NULL);
         CREATE FUNCTION audit_transfer_insert() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             INSERT INTO transfer_insert_sizes SELECT count(*), sum(octet_length(content)) FROM incoming
             HAVING count(*) > 0;
             RETURN NULL;
         END $$;
         CREATE TRIGGER audit_transfer_insert AFTER INSERT ON rh_messages
         REFERENCING NEW TABLE AS incoming FOR EACH STATEMENT EXECUTE FUNCTION audit_transfer_insert()",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn assert_insert_pieces(pool: &PgPool) -> TestResult {
    let pieces: Vec<(i64, i64)> =
        sqlx::query_as("SELECT rows, content_bytes FROM transfer_insert_sizes")
            .fetch_all(pool)
            .await?;
    assert!(pieces.len() >= 4, "{pieces:?}");
    assert!(pieces.contains(&(1, 4096)), "{pieces:?}");
    assert!(
        pieces
            .iter()
            .all(|&(rows, bytes)| rows <= 2 && (bytes <= 2048 || rows == 1)),
        "{pieces:?}"
    );
    assert_eq!(pieces.iter().map(|&(rows, _)| rows).sum::<i64>(), 7);
    Ok(())
}

async fn assert_counts(pool: &PgPool) -> TestResult {
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_message_counts_pending")
        .fetch_one(pool)
        .await?;
    assert_eq!(pending, 1);
    assert_message_counts(pool, &[("live".to_owned(), 4, 4)]).await?;
    let remaining: i64 = sqlx::query_scalar("SELECT rh_backfill_message_counts(100)")
        .fetch_one(pool)
        .await?;
    assert_eq!(remaining, 0);
    assert_message_counts(
        pool,
        &[("live".to_owned(), 4, 4), ("pending".to_owned(), 3, 3)],
    )
    .await
}

async fn assert_message_counts(pool: &PgPool, expected: &[(String, i64, i64)]) -> TestResult {
    let counts: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT c.session_key, mc.total, mc.queued FROM rh_message_counts mc
           JOIN rh_conversations c ON c.id = mc.conversation_id ORDER BY c.session_key",
    )
    .fetch_all(pool)
    .await?;
    assert_eq!(counts, expected);
    Ok(())
}

async fn assert_embed_jobs(pool: &PgPool) -> TestResult {
    let lengths: Vec<i32> = sqlx::query_scalar(
        "SELECT char_length(m.content) FROM graphile_worker._private_jobs j
         JOIN graphile_worker._private_tasks t ON t.id = j.task_id
         JOIN rh_messages m ON m.id::text = j.payload ->> 'targetId'
         WHERE t.identifier = 'embed-target' AND j.payload ->> 'targetTable' = 'ros_messages'
         ORDER BY char_length(m.content)",
    )
    .fetch_all(pool)
    .await?;
    assert_eq!(lengths, [5, 5, 5, 10, 1200, 1200, 4096]);
    Ok(())
}
