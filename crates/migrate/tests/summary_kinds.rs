#[expect(
    dead_code,
    reason = "the harness module is included whole by every test binary and this one uses a subset"
)]
mod common;

use std::path::Path;
use std::path::PathBuf;

use migrate::POSTGRES;
use rusqlite::Connection;
use rusqlite::OpenFlags;
use rusqlite::params;
use sqlx::PgPool;
use sqlx::Row as _;

type Row = (String, String, i64);
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const LEGACY: &[(&str, &str, i64)] = &[
    ("condensed one", "condensed", 1),
    ("condensed two", "condensed", 2),
    ("kept branch", "branch", 0),
    ("kept leaf", "leaf", 2),
    ("kept negative", "leaf", -3),
    ("kept root", "root", 1),
    ("unknown four", "rollup", 4),
    ("unknown zero", "rollup", 0),
];

const MAPPED: &[(&str, &str, i64)] = &[
    ("condensed one", "branch", 1),
    ("condensed two", "root", 2),
    ("kept branch", "branch", 0),
    ("kept leaf", "leaf", 2),
    ("kept negative", "leaf", -3),
    ("kept root", "root", 1),
    ("unknown four", "root", 4),
    ("unknown zero", "leaf", 0),
];

fn fresh() -> TestResult<(tempfile::TempDir, PathBuf)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    Ok((directory, path))
}

fn seed(path: &Path, rows: &[(&str, &str, i64)]) -> TestResult<(Vec<u8>, Vec<Row>)> {
    migrate::sqlite(path)?;
    let conn = Connection::open(path)?;
    for (content, kind, depth) in rows {
        conn.execute(
            "INSERT INTO ros_summaries (id, content, kind, depth, created_at)
             VALUES (?1, ?1, ?2, ?3, '2026-10-10T12:00:00Z')",
            params![content, kind, depth],
        )?;
    }
    conn.execute_batch(
        "PRAGMA wal_checkpoint(TRUNCATE);
         PRAGMA journal_mode = DELETE;
         PRAGMA user_version = 8;",
    )?;
    let before = read_rows(&conn)?;
    drop(conn);
    Ok((std::fs::read(path)?, before))
}

fn read_rows(conn: &Connection) -> Result<Vec<Row>, rusqlite::Error> {
    let mut statement =
        conn.prepare("SELECT content, kind, depth FROM ros_summaries ORDER BY content")?;
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect()
}

fn rows(path: &Path) -> TestResult<Vec<Row>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(read_rows(&conn)?)
}

fn owned(rows: &[(&str, &str, i64)]) -> Vec<Row> {
    rows.iter()
        .map(|(content, kind, depth)| ((*content).to_owned(), (*kind).to_owned(), *depth))
        .collect()
}

fn backup(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".v8.bak");
    PathBuf::from(name)
}

fn user_version(path: &Path) -> TestResult<i64> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

#[test]
fn legacy_summary_kinds_follow_depth_and_keep_the_v8_backup() -> TestResult {
    let (_directory, path) = fresh()?;
    let (original, before) = seed(&path, LEGACY)?;
    assert_eq!(before, owned(LEGACY));
    migrate::sqlite(&path)?;
    let bak = backup(&path);
    assert_eq!(std::fs::read(&bak)?, original);
    assert_eq!(rows(&bak)?, before);
    assert_eq!(rows(&path)?, owned(MAPPED));
    assert_eq!(user_version(&path)?, 34);
    let settled = std::fs::read(&path)?;
    let saved = std::fs::read(&bak)?;
    migrate::sqlite(&path)?;
    assert_eq!(std::fs::read(&path)?, settled);
    assert_eq!(std::fs::read(&bak)?, saved);
    let mut newer = path.as_os_str().to_os_string();
    newer.push(".v10.bak");
    assert!(!PathBuf::from(newer).exists());
    Ok(())
}

#[test]
fn legacy_summary_kind_below_zero_keeps_the_file_and_refuses_a_second_copy() -> TestResult {
    let (_directory, path) = fresh()?;
    let rows_in = [
        ("below zero", "condensed", -1),
        ("good row", "condensed", 1),
    ];
    let (_original, before) = seed(&path, &rows_in)?;
    match migrate::sqlite(&path) {
        Err(migrate::Error::UnmappedSummaryKind) => {}
        other => return Err(format!("expected UnmappedSummaryKind, got {other:?}").into()),
    }
    assert_eq!(user_version(&path)?, 8);
    assert_eq!(rows(&path)?, before);
    match migrate::sqlite(&path) {
        Err(migrate::Error::BackupExists { path: bak }) => {
            assert_eq!(bak, backup(&path));
        }
        other => return Err(format!("expected BackupExists, got {other:?}").into()),
    }
    assert_eq!(user_version(&path)?, 8);
    assert_eq!(rows(&path)?, before);
    Ok(())
}

async fn apply_through_22(pool: &PgPool) -> TestResult {
    for migration in POSTGRES
        .migrations
        .iter()
        .filter(|migration| migration.version < 23)
    {
        let sql: &'static str = Box::leak(migration.sql.as_str().to_string().into_boxed_str());
        sqlx::raw_sql(sql).execute(pool).await?;
    }
    Ok(())
}

async fn insert(pool: &PgPool, rows: &[(&str, &str, i64)]) -> TestResult {
    let mut tx = testkit::capture_tx(pool).await?;
    for (content, kind, depth) in rows {
        sqlx::query("INSERT INTO rh_summaries (content, kind, depth) VALUES ($1, $2, $3)")
            .bind(content)
            .bind(kind)
            .bind(*depth)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn stored(pool: &PgPool, view: bool) -> TestResult<Vec<Row>> {
    let sql = if view {
        "SELECT content, kind, depth FROM ros_summaries ORDER BY content"
    } else {
        "SELECT content, kind, depth FROM rh_summaries ORDER BY content"
    };
    let rows = sqlx::query(sql).fetch_all(pool).await?;
    let mut stored = Vec::new();
    for row in rows {
        stored.push((
            row.try_get("content")?,
            row.try_get("kind")?,
            i64::from(row.try_get::<i32, _>("depth")?),
        ));
    }
    Ok(stored)
}

async fn tracking(pool: &PgPool) -> TestResult<i64> {
    Ok(sqlx::query(
        "SELECT count(*)::bigint AS n FROM _rivetos_migrations WHERE name = '0023_summary_kinds.sql'",
    )
    .fetch_one(pool)
    .await?
    .try_get("n")?)
}

fn chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(next) = source {
        text.push(' ');
        text.push_str(&next.to_string());
        source = next.source();
    }
    text
}

#[sqlx::test(migrations = false)]
async fn legacy_summary_kinds_follow_depth_and_rerun_is_a_noop(pool: PgPool) -> TestResult {
    apply_through_22(&pool).await?;
    insert(&pool, LEGACY).await?;
    assert_eq!(stored(&pool, false).await?, owned(LEGACY));
    common::migrate_owned_by(&pool, testkit::OWNER).await?;
    let mapped = owned(MAPPED);
    assert_eq!(stored(&pool, false).await?, mapped);
    assert_eq!(stored(&pool, true).await?, mapped);
    common::migrate_owned_by(&pool, testkit::OWNER).await?;
    let Some(migration) = POSTGRES
        .migrations
        .iter()
        .find(|migration| migration.version == 23)
    else {
        return Err("migration 0023 is missing from the migrator".into());
    };
    let sql: &'static str = Box::leak(migration.sql.as_str().to_string().into_boxed_str());
    sqlx::raw_sql(sql).execute(&pool).await?;
    assert_eq!(stored(&pool, false).await?, mapped);
    assert_eq!(tracking(&pool).await?, 1);
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn legacy_summary_kind_below_zero_rolls_the_rewrite_back(pool: PgPool) -> TestResult {
    apply_through_22(&pool).await?;
    let seeded = [
        ("below zero", "condensed", -1),
        ("good row", "condensed", 1),
    ];
    insert(&pool, &seeded).await?;
    let before = stored(&pool, false).await?;
    let Err(error) = POSTGRES.run(&pool).await else {
        return Err("a negative depth must refuse the rewrite".into());
    };
    let text = chain(&error);
    assert!(
        text.contains("legacy summary kind has no depth mapping"),
        "{text}"
    );
    assert_eq!(stored(&pool, false).await?, before);
    assert_eq!(tracking(&pool).await?, 0);
    sqlx::query("UPDATE rh_summaries SET depth = 2 WHERE content = 'below zero'")
        .execute(&pool)
        .await?;
    common::migrate_owned_by(&pool, testkit::OWNER).await?;
    assert_eq!(
        stored(&pool, false).await?,
        owned(&[("below zero", "root", 2), ("good row", "branch", 1)])
    );
    assert_eq!(tracking(&pool).await?, 1);
    Ok(())
}
