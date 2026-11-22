mod owner;

use migrate::POSTGRES;
use sqlx::PgPool;
use sqlx::Row as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_migration_missing_below_the_newest_is_pending_until_migrate_applies_it(
    pool: PgPool,
) -> TestResult {
    sqlx::query("DROP INDEX idx_rh_messages_unembedded")
        .execute(&pool)
        .await?;
    sqlx::query("DELETE FROM _rivetos_migrations WHERE name = '0036_unembedded_messages.sql'")
        .execute(&pool)
        .await?;
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 36")
        .execute(&pool)
        .await?;

    let gapped = migrate::postgres_status_pool(&pool).await?;
    assert_eq!((gapped.current, gapped.pending), (82, 1));

    let migrated = migrate::postgres_pool(&pool).await?;
    assert_eq!((migrated.current, migrated.pending), (82, 0));
    let index: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('idx_rh_messages_unembedded')::text")
            .fetch_one(&pool)
            .await?;
    assert_eq!(index.as_deref(), Some("idx_rh_messages_unembedded"));
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn a_database_from_before_the_table_rename_upgrades_and_keeps_its_rows(
    pool: PgPool,
) -> TestResult {
    for migration in POSTGRES.migrations.iter().take(20) {
        let sql: &'static str = Box::leak(migration.sql.as_str().to_string().into_boxed_str());
        sqlx::raw_sql(sql).execute(&pool).await?;
    }
    sqlx::query(
        "INSERT INTO ros_conversations (session_key, agent, task_id)
         VALUES ('ts:legacy', 'rivet', '11111111-2222-3333-4444-555555555555')",
    )
    .execute(&pool)
    .await?;
    let removal = sqlx::query(
        "SELECT count(*)::bigint AS n FROM information_schema.columns
          WHERE table_name = 'ros_tags' AND column_name = 'removal_state'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(removal.try_get::<i64, _>("n").unwrap(), 1);

    owner::migrate_owned_by(&pool, testkit::OWNER).await?;

    let summaries_trgm =
        sqlx::query("SELECT indexdef FROM pg_indexes WHERE indexname = 'idx_ros_summaries_trgm'")
            .fetch_one(&pool)
            .await?;
    let indexdef: String = summaries_trgm.try_get("indexdef").unwrap();
    assert!(
        indexdef.contains("gin") && indexdef.contains("gin_trgm_ops"),
        "summaries trigram index must be a gin_trgm index: {indexdef}"
    );

    let legacy =
        sqlx::query("SELECT id, task_id FROM ros_conversations WHERE session_key = 'ts:legacy'")
            .fetch_one(&pool)
            .await?;
    let id: uuid::Uuid = legacy.try_get("id").unwrap();
    let task: Option<uuid::Uuid> = legacy.try_get("task_id").unwrap();
    assert_eq!(
        task,
        Some(uuid::uuid!("11111111-2222-3333-4444-555555555555"))
    );

    let renamed =
        sqlx::query("SELECT id, task_id FROM rh_conversations WHERE session_key = 'ts:legacy'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(renamed.try_get::<uuid::Uuid, _>("id").unwrap(), id);

    let removal = sqlx::query(
        "SELECT count(*)::bigint AS n FROM information_schema.columns
          WHERE table_name = 'rh_tags' AND column_name = 'removal_state'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(removal.try_get::<i64, _>("n").unwrap(), 1);
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn old_tasks_are_archived_and_their_jobs_dropped(pool: PgPool) -> TestResult {
    testkit::graphile_schema::install_graphile_worker(&pool).await?;
    for migration in POSTGRES
        .migrations
        .iter()
        .filter(|migration| migration.version < 40)
    {
        let sql: &'static str = Box::leak(migration.sql.as_str().to_string().into_boxed_str());
        sqlx::raw_sql(sql).execute(&pool).await?;
    }
    sqlx::raw_sql(
        "INSERT INTO rh_tasks (id, goal, executor, agent_id, origin, status, completed_at) VALUES
           ('00000000-0000-4000-8000-000000000001', 'beat', 'chat-loop', 'grok', 'heartbeat', 'queued', NULL),
           ('00000000-0000-4000-8000-000000000002', 'done', 'harness-session', 'claude', 'api', 'completed', '2026-10-01T00:00:00Z'),
           ('00000000-0000-4000-8000-000000000003', 'busy', 'chat-loop', 'kimi', 'tool', 'running', NULL);
         SELECT graphile_worker.add_job(name, '{}'::json, null::text, null::timestamptz, 1, null::text, 0, null::text[], 'replace')
           FROM unnest(ARRAY['run-task:node-a', 'run-task', 'heartbeat-grok-0', 'embed-target']) AS name;",
    )
    .execute(&pool)
    .await?;
    let archive_tasks = POSTGRES
        .migrations
        .iter()
        .find(|migration| migration.version == 40)
        .ok_or("0040")?;
    let sql: &'static str = Box::leak(archive_tasks.sql.as_str().to_string().into_boxed_str());
    sqlx::raw_sql(sql).execute(&pool).await?;
    let archived: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT goal, status, error FROM ros_tasks ORDER BY id")
            .fetch_all(&pool)
            .await?;
    let archive = "archived with the tasks refactor".to_owned();
    assert_eq!(
        archived,
        vec![
            (
                "beat".to_owned(),
                "killed".to_owned(),
                Some(archive.clone())
            ),
            ("done".to_owned(), "completed".to_owned(), None),
            ("busy".to_owned(), "killed".to_owned(), Some(archive)),
        ]
    );
    let fresh: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_tasks")
        .fetch_one(&pool)
        .await?;
    assert_eq!(fresh, 0);
    let jobs: Vec<String> = sqlx::query_scalar(
        "SELECT t.identifier FROM graphile_worker._private_jobs j
           JOIN graphile_worker._private_tasks t ON t.id = j.task_id ORDER BY 1",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(jobs, vec!["embed-target".to_owned()]);
    let outcomes: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('ros_task_outcomes_v')::text")
            .fetch_one(&pool)
            .await?;
    assert_eq!(outcomes, None);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_ros_views_read_and_write_the_rh_tables(pool: PgPool) -> TestResult {
    POSTGRES.run(&pool).await?;

    let mut tx = testkit::capture_tx(&pool).await?;
    sqlx::query(
        "INSERT INTO rh_conversations (session_key, agent, title, owner_user_id)
         VALUES ('view:one', 'rivet', 't1', 'owner')",
    )
    .execute(&mut *tx)
    .await?;
    let conversation: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM rh_conversations WHERE session_key = 'view:one'")
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query(
        "INSERT INTO rh_messages (conversation_id, agent, channel, role, content, owner_user_id)
         VALUES ($1, 'rivet', 'cli', 'user', 'through the view', 'owner')",
    )
    .bind(conversation)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let through_view: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ros_messages
          WHERE conversation_id = $1 AND content = 'through the view'",
    )
    .bind(conversation)
    .fetch_one(&pool)
    .await?;
    assert_eq!(through_view, 1);

    let view_row = sqlx::query("SELECT title FROM ros_conversations WHERE id = $1")
        .bind(conversation)
        .fetch_one(&pool)
        .await?;
    assert_eq!(view_row.try_get::<String, _>("title").unwrap(), "t1");

    let mut tx = testkit::capture_tx(&pool).await?;
    let insert_through_view: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO ros_conversations (session_key, agent, title, owner_user_id)
         VALUES ('view:two', 'rivet', 't2', 'owner') RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO ros_messages (conversation_id, agent, channel, role, content, owner_user_id)
         VALUES ($1, 'rivet', 'cli', 'user', 'job fires through the view', 'owner')",
    )
    .bind(insert_through_view)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let stored = sqlx::query(
        "SELECT count(*)::bigint AS n FROM rh_conversations WHERE id = $1 AND title = 't2'",
    )
    .bind(insert_through_view)
    .fetch_one(&pool)
    .await?;
    assert_eq!(stored.try_get::<i64, _>("n").unwrap(), 1);

    let job: Option<String> = sqlx::query_scalar(
        "SELECT t.identifier FROM graphile_worker._private_jobs j
          JOIN graphile_worker._private_tasks t ON t.id = j.task_id",
    )
    .fetch_optional(&pool)
    .await?
    .flatten();
    assert_eq!(job.as_deref(), Some("embed-target"));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_missing_tracking_row_is_a_typed_error(pool: PgPool) -> TestResult {
    sqlx::query("DELETE FROM _rivetos_migrations WHERE name = '0004_task_eval.sql'")
        .execute(&pool)
        .await?;
    match migrate::postgres_pool(&pool).await {
        Err(migrate::Error::TrackingMissing { missing }) => {
            assert!(
                missing.contains("0004_task_eval.sql"),
                "the error must name the missing row: {missing}"
            );
            assert!(
                !missing.contains("0001_baseline.sql"),
                "present rows must not be named: {missing}"
            );
        }
        other => panic!("expected TrackingMissing, got {other:?}"),
    }
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn a_page_is_bound_to_the_root_topic_with_its_slug(pool: PgPool) -> TestResult {
    for migration in POSTGRES.migrations.iter().take(26) {
        let sql: &'static str = Box::leak(migration.sql.as_str().to_string().into_boxed_str());
        sqlx::raw_sql(sql).execute(&pool).await?;
    }
    sqlx::raw_sql(
        "INSERT INTO rh_topics (id, slug, name, parent_id, created_at, updated_at) VALUES
           ('00000000-0000-0000-0000-000000000001', 'mesh', 'Mesh', NULL, now(), now()),
           ('00000000-0000-0000-0000-000000000002', 'certs', 'Certs',
            '00000000-0000-0000-0000-000000000001', now(), now());
         INSERT INTO rh_wiki_topics (slug, title) VALUES ('mesh', 'Mesh'), ('certs', 'Certs');",
    )
    .execute(&pool)
    .await?;

    let last = POSTGRES.migrations.get(26).unwrap();
    let sql: &'static str = Box::leak(last.sql.as_str().to_string().into_boxed_str());
    sqlx::raw_sql(sql).execute(&pool).await?;

    let bound: Vec<(String, Option<sqlx::types::Uuid>)> =
        sqlx::query_as("SELECT slug, topic_id FROM rh_wiki_topics ORDER BY slug")
            .fetch_all(&pool)
            .await?;
    assert_eq!(
        bound,
        vec![
            ("certs".to_owned(), None),
            (
                "mesh".to_owned(),
                Some("00000000-0000-0000-0000-000000000001".parse()?)
            ),
        ],
        "only a root topic takes the page with its slug"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_session_keeps_one_startup_context(pool: PgPool) -> TestResult {
    let insert = "INSERT INTO rh_session_startup (session_id, topic_id, tokens, sections, created_at)
                  VALUES ('claude-code:one', '00000000-0000-0000-0000-000000000001', 0, '[]', now())";
    sqlx::raw_sql(insert).execute(&pool).await?;
    assert!(
        sqlx::raw_sql(insert).execute(&pool).await.is_err(),
        "a session keeps one startup context"
    );
    let last = POSTGRES.migrations.get(27).unwrap();
    let sql: &'static str = Box::leak(last.sql.as_str().to_string().into_boxed_str());
    sqlx::raw_sql(sql).execute(&pool).await?;
    let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_session_startup")
        .fetch_one(&pool)
        .await?;
    assert_eq!(kept, 1, "a second run changes nothing");
    Ok(())
}

async fn migrate_through_75(pool: &PgPool) -> TestResult {
    let first_top_down = POSTGRES
        .migrations
        .partition_point(|migration| migration.version < 76);
    let legacy = POSTGRES
        .migrations
        .get(..first_top_down)
        .ok_or("missing migrations through 75")?;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Borrowed(legacy),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before.run(pool).await?;
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn top_down_upgrade_fails_fast_with_a_reader(pool: PgPool) -> TestResult {
    check_contended_top_down_upgrade(&pool, "SELECT id FROM rh_conversations").await
}

#[sqlx::test(migrations = false)]
async fn top_down_upgrade_fails_fast_with_a_writer(pool: PgPool) -> TestResult {
    check_contended_top_down_upgrade(
        &pool,
        "INSERT INTO rh_conversations (session_key, agent, owner_user_id)
         VALUES ('cli:lock-fixture', 'rivet', 'alice')",
    )
    .await
}

async fn check_contended_top_down_upgrade(pool: &PgPool, hold: &'static str) -> TestResult {
    use std::time::Duration;

    migrate_through_75(pool).await?;
    sqlx::query(
        "CREATE UNIQUE INDEX CONCURRENTLY rh_summary_one_session
           ON rh_summaries (conversation_id) WHERE kind = 'session'",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX CONCURRENTLY rh_summary_sources_order
           ON rh_summary_sources (summary_id, ordinal)",
    )
    .execute(pool)
    .await?;
    let mut blocker = pool.begin().await?;
    sqlx::query(hold).execute(&mut *blocker).await?;
    let mut migration = pool.acquire().await?;
    let migrator_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *migration)
        .await?;
    let mut reader = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout = '500ms'")
        .execute(&mut *reader)
        .await?;

    let started = async {
        let mut ticks = tokio::time::interval(Duration::from_millis(10));
        loop {
            ticks.tick().await;
            let applying: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1
                  AND starts_with(query, '-- Runtime path: crates/migrate/migrations/0076'))",
            )
            .bind(migrator_pid)
            .fetch_one(pool)
            .await?;
            if applying {
                return Ok::<_, sqlx::Error>(());
            }
        }
    };
    let (result, read) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(4), migrate::postgres_apply(migration)),
        async {
            tokio::time::timeout(Duration::from_secs(2), started)
                .await
                .map_err(|_| "the migrator did not reach 0076")??;
            sqlx::query("SELECT id FROM rh_conversations")
                .execute(&mut *reader)
                .await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        }
    );
    assert!(read.is_ok(), "a fresh reader queued behind 0076: {read:?}");
    let Ok(Err(error)) = result else {
        return Err(format!("expected 0076 to fail fast, got {result:?}").into());
    };
    assert!(
        matches!(&error,
            migrate::Error::PostgresMigrate(sqlx::migrate::MigrateError::ExecuteMigration(
                sqlx::Error::Database(db), 76,
            )) if db.code().as_deref() == Some("55P03")
        ),
        "expected retryable lock contention, got {error:?}",
    );
    assert!(error.to_string().contains("safe to rerun"));
    reader.rollback().await?;
    blocker.rollback().await?;
    assert_eq!(migrate::postgres_pool(pool).await?.current, 82);
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn top_down_upgrade_from_postgres_75_preserves_legacy_kinds(pool: PgPool) -> TestResult {
    migrate_through_75(&pool).await?;
    let pending = migrate::postgres_status_pool(&pool).await?;
    assert_eq!(
        (pending.current, pending.required, pending.pending),
        (75, 82, 7)
    );
    sqlx::raw_sql(
        "INSERT INTO rh_conversations (id, session_key, agent, owner_user_id)
         VALUES ('10000000-0000-4000-8000-000000000001', 'cli:legacy', 'rivet', 'alice');
         INSERT INTO rh_summaries (id, conversation_id, kind, depth, content, owner_user_id)
         VALUES ('10000000-0000-4000-8000-000000000002', '10000000-0000-4000-8000-000000000001',
                 'leaf', 2, 'Old fixture note', 'alice');",
    )
    .execute(&pool)
    .await?;
    let filenode_before: u32 = sqlx::query_scalar::<_, sqlx::postgres::types::Oid>(
        "SELECT pg_relation_filenode('rh_summaries'::regclass)",
    )
    .fetch_one(&pool)
    .await?
    .0;
    let status = migrate::postgres_pool(&pool).await?;
    assert_eq!(
        (status.current, status.required, status.pending),
        (82, 82, 0)
    );
    let filenode_after: sqlx::postgres::types::Oid =
        sqlx::query_scalar("SELECT pg_relation_filenode('rh_summaries'::regclass)")
            .fetch_one(&pool)
            .await?;
    assert_eq!(filenode_before, filenode_after.0);
    let row: (String, i32, Option<i32>, bool) =
        sqlx::query_as("SELECT kind, depth, summary_ordinal, index_needs_repair FROM rh_summaries")
            .fetch_one(&pool)
            .await?;
    assert_eq!(row, ("leaf".to_owned(), 2, None, false));
    let marker: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0076_top_down_summaries.sql')",
    ).fetch_one(&pool).await?;
    let opening: (String, bool, bool) =
        sqlx::query_as("SELECT content, filing_armed, title_armed FROM rh_summary_openings")
            .fetch_one(&pool)
            .await?;
    assert_eq!(opening, ("Old fixture note".to_owned(), false, false));
    assert!(marker);
    Ok(())
}

#[test]
fn top_down_upgrade_from_sqlite_35_preserves_legacy_kinds_and_matches_fresh() -> TestResult {
    use rusqlite::Connection;

    let dir = tempfile::tempdir()?;
    let fresh = dir.path().join("fresh.sqlite");
    let upgraded = dir.path().join("upgraded.sqlite");
    migrate::sqlite(&fresh)?;
    let conn = Connection::open(&fresh)?;
    conn.execute("VACUUM INTO ?1", [upgraded.to_string_lossy().as_ref()])?;
    let old = Connection::open(&upgraded)?;
    old.execute_batch(
        "DROP VIEW rh_transcript_times;
         DROP INDEX rh_transcript_seek; DROP INDEX rh_transcript_recent;
         DROP TABLE IF EXISTS rh_summary_capture_clock;
         DROP TABLE rh_summary_checkpoints; DROP TABLE rh_summary_runs;
         DROP TABLE rh_summary_state; DROP TRIGGER rh_disarm_trashed_openings; DROP TABLE rh_summary_openings;
         ALTER TABLE ros_conversations DROP COLUMN title_from_capture;
         DROP TABLE rh_summary_conversion; DROP TABLE rh_summary_conversion_items;
         DROP TABLE ros_summary_deliveries;
         DROP INDEX rh_summary_one_session; DROP INDEX rh_summary_sources_order;
         ALTER TABLE ros_summary_sources DROP COLUMN render_key;
         ALTER TABLE ros_summaries DROP COLUMN summary_ordinal;
         ALTER TABLE ros_summaries DROP COLUMN index_needs_repair;
         ALTER TABLE ros_summaries DROP COLUMN issued_part_ordinal;
         ALTER TABLE ros_summaries DROP COLUMN input_token_estimate;
         ALTER TABLE ros_summaries DROP COLUMN source_first;
         ALTER TABLE ros_summaries DROP COLUMN source_last;
         ALTER TABLE ros_summaries DROP COLUMN range_verified;
         ALTER TABLE ros_summaries DROP COLUMN input_key;
         ALTER TABLE ros_summaries DROP COLUMN index_key;
         ALTER TABLE ros_summaries DROP COLUMN quality_flags;
         ALTER TABLE ros_summaries DROP COLUMN section_char_start;
         ALTER TABLE ros_summaries DROP COLUMN section_char_end;
         INSERT INTO ros_summaries (id, kind, depth, content, created_at)
           VALUES ('legacy', 'branch', 2, 'Old fixture note', '2026-10-10T00:00:00.000Z');
         INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at)
         VALUES ('kept','cli:kept','fixture','2026-10-10T00:00:00Z','2026-10-10T00:00:00Z'), ('trash','cli:trashed','fixture','2026-10-10T00:00:00Z','2026-10-10T00:00:00Z');
         INSERT INTO ros_summaries (id, conversation_id, kind, depth, content, created_at)
         VALUES ('legacy-kept','kept','leaf',0,'Invented packing review','2026-10-10'),
                ('legacy-trash','trash','leaf',0,'Invented list of fasteners','2026-10-10');
         INSERT INTO rh_session_trash (session_key, trashed_at) VALUES ('cli:trashed','2026-10-11');
         PRAGMA user_version = 35;",
    )?;
    drop(old);
    let pending = migrate::sqlite_status(&upgraded)?;
    assert_eq!(
        (pending.current, pending.required, pending.pending),
        (35, 41, 6)
    );
    let status = migrate::sqlite(&upgraded)?;
    assert_eq!(
        (status.current, status.required, status.pending),
        (41, 41, 0)
    );
    let up = Connection::open(&upgraded)?;
    let row: (String, i32, Option<i32>, bool) = up.query_row(
        "SELECT kind, depth, summary_ordinal, index_needs_repair FROM ros_summaries WHERE id = 'legacy'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let opening: (String, bool, bool, bool) = up.query_row(
        "SELECT conversation_id, filing_armed, title_armed, historical_pending FROM rh_summary_openings",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!(opening, ("kept".to_owned(), false, false, true));
    up.execute(
        "INSERT INTO rh_summary_conversion_items (conversation_id, last_input_at, estimated_tokens, state)
         VALUES ('kept','2026-10-11T00:00:00Z',1,'pending')",
        [],
    )?;
    let next: String = up.query_row(
        "SELECT next_at FROM rh_summary_conversion_items",
        [],
        |row| row.get(0),
    )?;
    assert!(next.parse::<jiff::Timestamp>().is_ok());
    assert_eq!(row, ("branch".to_owned(), 2, None, false));
    assert_eq!(sqlite_definitions(&conn)?, sqlite_definitions(&up)?);
    Ok(())
}

fn sqlite_definitions(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<(String, String)>> {
    conn.prepare("SELECT name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_passages_use_store_hnsw_options(pool: PgPool) -> TestResult {
    let options: Option<Vec<String>> = sqlx::query_scalar(
        "SELECT reloptions FROM pg_class WHERE oid = 'rh_summary_passages_hnsw'::regclass",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        options.as_deref(),
        Some(["m=32".to_owned(), "ef_construction=120".to_owned()].as_slice())
    );
    Ok(())
}
