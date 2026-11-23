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

fn definitions(path: &Path) -> rusqlite::Result<Vec<(String, String, String)>> {
    let conn = rusqlite::Connection::open(path)?;
    let mut objects = conn.prepare(
        "SELECT name, type, CASE type WHEN 'table' THEN '' ELSE coalesce(sql, '') END
         FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name, type",
    )?;
    let mut found: Vec<(String, String, String)> = objects
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut columns = conn.prepare(
        "SELECT m.name, c.name, c.type || ' ' || c.\"notnull\" || ' ' || coalesce(c.dflt_value, '') || ' ' || c.pk
         FROM sqlite_master m JOIN pragma_table_info(m.name) c
         WHERE m.type = 'table' AND m.name NOT LIKE 'sqlite_%' ORDER BY m.name, c.name",
    )?;
    let described = columns
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?;
    found.extend(described);
    Ok(found)
}

#[test]
fn the_backup_holds_changes_still_in_the_write_ahead_log() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    std::fs::copy(
        testkit::paths::recording("memory-sqlite/upgrade-v5.sqlite"),
        &path,
    )?;
    let holder = rusqlite::Connection::open(&path)?;
    holder.execute_batch("PRAGMA journal_mode = wal; PRAGMA wal_autocheckpoint = 0;")?;
    let writer = rusqlite::Connection::open(&path)?;
    writer.execute_batch("PRAGMA wal_autocheckpoint = 0; CREATE TABLE late_note (body TEXT)")?;
    drop(writer);
    assert!(std::fs::metadata(dir.path().join("memory.sqlite-wal"))?.len() > 0);
    migrate::sqlite(&path)?;
    drop(holder);
    let backup = rusqlite::Connection::open(dir.path().join("memory.sqlite.v5.bak"))?;
    let kept: i64 = backup.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name = 'late_note'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(kept, 1);
    Ok(())
}

#[test]
fn a_file_another_reader_holds_is_not_backed_up_or_upgraded() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    std::fs::copy(
        testkit::paths::recording("memory-sqlite/upgrade-v5.sqlite"),
        &path,
    )?;
    let reader = rusqlite::Connection::open(&path)?;
    reader.execute_batch("PRAGMA journal_mode = wal; PRAGMA wal_autocheckpoint = 0;")?;
    let writer = rusqlite::Connection::open(&path)?;
    writer.execute_batch("PRAGMA wal_autocheckpoint = 0; CREATE TABLE first_note (body TEXT)")?;
    reader.execute_batch("BEGIN; SELECT count(*) FROM sqlite_master;")?;
    writer.execute_batch("CREATE TABLE second_note (body TEXT)")?;
    drop(writer);
    match migrate::sqlite(&path) {
        Err(migrate::Error::InUse { path: reported }) => assert_eq!(reported, path),
        other => return Err(format!("expected InUse, got {other:?}").into()),
    }
    assert!(!dir.path().join("memory.sqlite.v5.bak").try_exists()?);
    reader.execute_batch("COMMIT")?;
    drop(reader);
    assert_eq!(user_version(&path)?, 5);
    Ok(())
}
