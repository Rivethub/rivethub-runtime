use kernel::owner::Owner;
use memory_sqlite::SqliteStore;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::{Direction, pg, run, verify};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn store(dir: &tempfile::TempDir, name: &str) -> Result<SqliteStore, Box<dyn std::error::Error>> {
    let path = dir.path().join(name);
    migrate::sqlite(&path)?;
    Ok(SqliteStore::open(path)?)
}

async fn fixture(store: &SqliteStore) -> TestResult {
    store
        .with_conn(|conn| {
            conn.execute_batch(super::fixture::MEMORY)
                .map_err(memory_sqlite::SqliteError::Pragma)
        })
        .await?;
    Ok(())
}

async fn authorization(pool: &PgPool) -> TestResult {
    sqlx::query!("INSERT INTO rh_spaces (id, kind, name) VALUES ('team', 'team', 'Team')")
        .execute(pool)
        .await?;
    sqlx::query!(
        "INSERT INTO rh_space_members (space_id, user_id, role) VALUES ('team', 'alice', 'member')"
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn other_owner(pool: &PgPool) -> TestResult {
    sqlx::query!(
        "INSERT INTO rh_conversations (session_key, agent, owner_user_id)
        VALUES ('other-session', 'codex', 'bob')"
    )
    .execute(pool)
    .await?;
    sqlx::query!(
        "INSERT INTO rh_review_marks (owner_user_id, item_key, state, at)
        VALUES ('bob', 'other-review', 'decided', now())"
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn round_trip(pool: &PgPool, original: &SqliteStore, restored: &SqliteStore) -> TestResult {
    let owner = Owner::new("alice".into())?;
    let token = CancellationToken::new();
    let preview = run(
        pool,
        original,
        [&owner, &owner],
        Direction::ToPostgres,
        (true, None),
        &token,
    )
    .await?;
    assert!(!preview.committed);
    let first = run(
        pool,
        original,
        [&owner, &owner],
        Direction::ToPostgres,
        (false, None),
        &token,
    )
    .await?;
    assert_eq!(preview.added, first.added);
    for (table, count) in &first.read {
        assert!(*count > 0, "fixture must cover {table}");
    }
    let back = run(
        pool,
        restored,
        [&owner, &owner],
        Direction::ToSqlite,
        (false, None),
        &token,
    )
    .await?;
    assert_eq!(back.read, first.read);
    verify::assert_same(
        &verify::sqlite(pool, original).await?,
        &verify::sqlite(pool, restored).await?,
    );
    let equal_original = run(
        pool,
        original,
        [&owner, &owner],
        Direction::ToSqlite,
        (false, None),
        &token,
    )
    .await?;
    assert_eq!(equal_original.added.values().sum::<u64>(), 0);
    let equal_remote = run(
        pool,
        restored,
        [&owner, &owner],
        Direction::ToPostgres,
        (false, None),
        &token,
    )
    .await?;
    assert_eq!(equal_remote.read, first.read);
    assert_eq!(equal_remote.added.values().sum::<u64>(), 0);
    let equal_restored = run(
        pool,
        restored,
        [&owner, &owner],
        Direction::ToSqlite,
        (false, None),
        &token,
    )
    .await?;
    assert_eq!(equal_restored.added.values().sum::<u64>(), 0);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn transfers_every_catalog_relation_both_ways(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let original = store(&directory, "original.sqlite")?;
    let restored = store(&directory, "restored.sqlite")?;
    fixture(&original).await?;
    authorization(&pool).await?;
    round_trip(&pool, &original, &restored).await?;
    let standalone = store(&directory, "standalone.sqlite")?;
    project_hub_access(&pool, &standalone).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn refuses_divergent_destinations_atomically(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let local = store(&directory, "original.sqlite")?;
    fixture(&local).await?;
    authorization(&pool).await?;
    let owner = Owner::new("alice".into())?;
    let token = CancellationToken::new();
    run(
        &pool,
        &local,
        [&owner, &owner],
        Direction::ToPostgres,
        (false, None),
        &token,
    )
    .await?;
    sqlx::query!("UPDATE rh_conversations SET title = 'newer title', updated_at = now() WHERE owner_user_id = 'alice'")
        .execute(&pool).await?;
    let error = run(
        &pool,
        &local,
        [&owner, &owner],
        Direction::ToPostgres,
        (false, None),
        &token,
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("conflict in rh_conversations"),
        "{error}"
    );
    let error = run(
        &pool,
        &local,
        [&owner, &owner],
        Direction::ToSqlite,
        (false, None),
        &token,
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("conflict in ros_conversations"),
        "{error}"
    );
    Ok(())
}

async fn fresh_hub(
    pool: &PgPool,
    local: &SqliteStore,
    expected: &std::collections::BTreeMap<String, Vec<String>>,
) -> TestResult {
    let database = format!("rh_transfer_{}", uuid::Uuid::new_v4().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(pool)
        .await?;
    let options = pool.connect_options().as_ref().clone().database(&database);
    let destination = sqlx::postgres::PgPoolOptions::new()
        .connect_with(options)
        .await?;
    let result = verify_hub(&destination, local, expected).await;
    destination.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE {database}")))
        .execute(pool)
        .await?;
    result
}

async fn verify_hub(
    pool: &PgPool,
    source: &SqliteStore,
    expected: &std::collections::BTreeMap<String, Vec<String>>,
) -> TestResult {
    migrate::postgres_pool(pool).await?;
    authorization(pool).await?;
    let owner = Owner::new("alice".into())?;
    let local_owner = Owner::new("standalone".into())?;
    let token = CancellationToken::new();
    let copied = run(
        pool,
        source,
        [&owner, &local_owner],
        Direction::ToPostgres,
        (false, None),
        &token,
    )
    .await?;
    verify::assert_same(expected, &verify::postgres(pool).await?);
    assert_eq!(copied.added.get("rh_conversations"), Some(&1));
    let checked = run(
        pool,
        source,
        [&owner, &local_owner],
        Direction::ToSqlite,
        (true, None),
        &token,
    )
    .await?;
    assert_eq!(checked.added.values().sum::<u64>(), 0);
    assert_eq!(checked.read, copied.read);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn refuses_to_resurrect_a_deleted_session(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let local = store(&directory, "original.sqlite")?;
    fixture(&local).await?;
    authorization(&pool).await?;
    sqlx::raw_sql(
        "INSERT INTO rh_session_tombstones (session_key, deleted_at, owner_user_id)
        VALUES ('codex:session', now(), 'alice')",
    )
    .execute(&pool)
    .await?;
    let owner = Owner::new("alice".into())?;
    let token = CancellationToken::new();
    let error = run(
        &pool,
        &local,
        [&owner, &owner],
        Direction::ToPostgres,
        (false, None),
        &token,
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("session deletion conflict"),
        "{error}"
    );
    let result = sqlx::query_scalar!("SELECT count(*) FROM rh_tasks")
        .fetch_one(&pool)
        .await?;
    assert_eq!(result, Some(0));
    Ok(())
}

async fn project_hub_access(pool: &PgPool, local: &SqliteStore) -> TestResult {
    sqlx::raw_sql(
        "INSERT INTO rh_message_access VALUES
        ('00000000-0000-0000-0000-000000000003', 7, '2026-01-02T00:00:00Z')",
    )
    .execute(pool)
    .await?;
    let hub_owner = Owner::new("alice".into())?;
    let local_owner = Owner::new("standalone".into())?;
    let token = CancellationToken::new();
    run(
        pool,
        local,
        [&hub_owner, &local_owner],
        Direction::ToSqlite,
        (false, None),
        &token,
    )
    .await?;
    let access_count = local
        .with_conn(|conn| {
            conn.query_row(
                "SELECT access_count FROM ros_messages WHERE content = 'First'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(memory_sqlite::SqliteError::Pragma)
        })
        .await?;
    assert_eq!(access_count, 7);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn export_excludes_other_people_and_global_catalogs(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let local = store(&directory, "filtered.sqlite")?;
    other_owner(&pool).await?;
    sqlx::raw_sql(super::fixture::FOREIGN_MEMORY)
        .execute(&pool)
        .await?;
    let owner = Owner::new("alice".into())?;
    let exported = run(
        &pool,
        &local,
        [&owner, &owner],
        Direction::ToSqlite,
        (false, None),
        &CancellationToken::new(),
    )
    .await?;
    assert_eq!(
        exported.read.values().copied().sum::<u64>(),
        0,
        "{exported:?}"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn postgres_round_trip_matches_every_row_with_two_people(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let initial = store(&directory, "fixture.sqlite")?;
    let clean = store(&directory, "clean.sqlite")?;
    let exported = store(&directory, "export.sqlite")?;
    fixture(&initial).await?;
    authorization(&pool).await?;
    let owner = Owner::new("alice".into())?;
    let standalone = Owner::new("standalone".into())?;
    let token = CancellationToken::new();
    run(
        &pool,
        &initial,
        [&owner, &owner],
        Direction::ToPostgres,
        (false, None),
        &token,
    )
    .await?;
    sqlx::raw_sql(super::fixture::HUB_MEMORY)
        .execute(&pool)
        .await?;
    let expected = verify::postgres(&pool).await?;
    run(
        &pool,
        &clean,
        [&owner, &standalone],
        Direction::ToSqlite,
        (false, None),
        &token,
    )
    .await?;
    add_second_person(&pool, &directory).await?;
    run(
        &pool,
        &exported,
        [&owner, &standalone],
        Direction::ToSqlite,
        (false, None),
        &token,
    )
    .await?;
    verify::assert_same(
        &verify::sqlite(&pool, &clean).await?,
        &verify::sqlite(&pool, &exported).await?,
    );
    fresh_hub(&pool, &exported, &expected).await
}

async fn add_second_person(pool: &PgPool, directory: &tempfile::TempDir) -> TestResult {
    let cedar = store(directory, "cedar.sqlite")?;
    let rows = super::fixture::MEMORY
        .replace("00000000-0000-0000-0000-", "20000000-0000-0000-0000-")
        .replace("alice", "bob")
        .replace("'team'", "'cedar-team'")
        .replace(
            "'cedar-team', 'cedar-team', 'Team'",
            "'cedar-team', 'team', 'Cedar team'",
        )
        .replace("'codex:", "'codex:cedar-")
        .replace("'one'", "'cedar-preset'")
        .replace("'Writer'", "'Cedar writer'")
        .replace("'test-call'", "'cedar-call'")
        .replace("'discarded'", "'cedar-discarded'")
        .replace("'child'", "'cedar-child'")
        .replace("'parent'", "'cedar-parent'")
        .replace("'quill'", "'cedar-quill'")
        .replace("'the-transfer'", "'cedar-the-transfer'")
        .replace("'transfer'", "'cedar-transfer'")
        .replace("'move'", "'cedar-move'")
        .replace("'candidate'", "'cedar-candidate'");
    cedar
        .with_conn(move |conn| {
            conn.execute_batch(&rows)
                .map_err(memory_sqlite::SqliteError::Pragma)
        })
        .await?;
    sqlx::raw_sql(
        "INSERT INTO rh_spaces VALUES ('cedar-team', 'team', 'Cedar team');
                   INSERT INTO rh_space_members VALUES ('cedar-team', 'bob', 'member')",
    )
    .execute(pool)
    .await?;
    let owner = Owner::new("bob".into())?;
    run(
        pool,
        &cedar,
        [&owner, &owner],
        Direction::ToPostgres,
        (false, None),
        &CancellationToken::new(),
    )
    .await?;
    other_owner(pool).await?;
    sqlx::raw_sql(super::fixture::FOREIGN_MEMORY)
        .execute(pool)
        .await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn postgres_transfer_defers_self_references(pool: PgPool) -> TestResult {
    let mut transaction = pg::begin(&pool, Direction::ToPostgres).await?;
    sqlx::raw_sql(super::fixture::UNSORTED_CHILDREN)
        .execute(&mut *transaction)
        .await?;
    sqlx::raw_sql(super::fixture::UNSORTED_PARENTS)
        .execute(&mut *transaction)
        .await?;
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *transaction)
        .await?;
    transaction.rollback().await?;
    Ok(())
}

#[path = "regressions.rs"]
mod regressions;

#[path = "passage_tests.rs"]
mod passage_tests;
