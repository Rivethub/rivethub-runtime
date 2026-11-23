use std::path::Path;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn an_interrupted_upgrade_resumes_from_the_last_finished_step() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    std::fs::copy(
        testkit::paths::recording("memory-sqlite/upgrade-v5.sqlite"),
        &path,
    )?;
    rusqlite::Connection::open(&path)?.execute_batch("CREATE TABLE rh_agent_presets (obstacle)")?;
    match migrate::sqlite(&path) {
        Err(migrate::Error::SqliteUpgrade { version, .. }) => assert_eq!(version, 44),
        other => return Err(format!("expected step 44 to fail, got {other:?}").into()),
    }
    assert_eq!(user_version(&path)?, 43);
    rusqlite::Connection::open(&path)?.execute_batch("DROP TABLE rh_agent_presets")?;
    let status = migrate::sqlite(&path)?;
    assert_eq!(
        (status.current, status.pending),
        (memory_sqlite::SCHEMA_VERSION, 0)
    );
    for kept in ["memory.sqlite.v5.bak", "memory.sqlite.v43.bak"] {
        assert!(dir.path().join(kept).try_exists()?, "{kept}");
    }
    let fresh = dir.path().join("fresh.sqlite");
    migrate::sqlite(&fresh)?;
    assert_eq!(definitions(&path)?, definitions(&fresh)?);
    Ok(())
}

fn user_version(path: &Path) -> rusqlite::Result<i64> {
    rusqlite::Connection::open(path)?.query_row("PRAGMA user_version", [], |row| row.get(0))
}

fn definitions(path: &Path) -> rusqlite::Result<Vec<(String, String)>> {
    let conn = rusqlite::Connection::open(path)?;
    let mut statement = conn.prepare(
        "SELECT name, type FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name, type",
    )?;
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}
