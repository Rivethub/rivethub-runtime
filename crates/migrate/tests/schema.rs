#[expect(dead_code)]
mod common;

use std::fs;

use common::TestResult;
use common::connection;
use common::fixture_copy;
use common::read_column_types;
use common::read_legacy_master_entries;
use common::read_pragmas;
use common::read_row_count;
use common::read_table_columns;
use common::schema_recording;
use sha2::Digest;
use sha2::Sha256;
use tempfile::tempdir;

#[test]
fn the_shipped_schema_text_is_the_recorded_text() -> TestResult<()> {
    let recording = schema_recording()?;
    let shipped = include_str!("../src/schema/schema_v7.sql");
    assert_eq!(shipped, recording.schema_text);
    let mut hasher = Sha256::new();
    hasher.update(shipped.as_bytes());
    assert_eq!(
        format!("{:x}", hasher.finalize()),
        recording.schema_text_sha256
    );
    Ok(())
}

#[test]
fn a_fresh_file_preserves_the_recorded_legacy_schema() -> TestResult<()> {
    let recording = schema_recording()?;
    let dir = tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let conn = connection(&path)?;
    common::assert_upgraded_pragmas(&read_pragmas(&conn)?, &recording.fresh.pragmas);
    assert_eq!(
        read_legacy_master_entries(&conn)?,
        recording.fresh.sqlite_master,
        "sqlite_master of a fresh file"
    );
    for (table, expected) in &recording.fresh.table_info {
        assert_eq!(
            read_table_columns(&conn, table)?,
            *expected,
            "table_info of {table}"
        );
    }
    Ok(())
}

#[test]
fn the_v7_fixture_preserves_legacy_schema_and_rows() -> TestResult<()> {
    let recording = schema_recording()?;
    let fixture = fixture_copy("fixture-v7.sqlite")?;
    migrate::sqlite(fixture.path())?;
    let conn = connection(fixture.path())?;
    common::assert_upgraded_pragmas(&read_pragmas(&conn)?, &recording.fixture.pragmas);
    assert!(
        recording
            .fixture
            .row_counts
            .keys()
            .eq(recording.fixture.column_types.keys()),
        "the recording covers the same tables in row counts and typeof"
    );
    assert_eq!(
        read_legacy_master_entries(&conn)?,
        recording.fresh.sqlite_master,
        "sqlite_master of the fixture equals the fresh schema"
    );
    for (table, expected) in &recording.fixture.row_counts {
        assert_eq!(read_row_count(&conn, table)?, *expected, "rows of {table}");
    }
    for (table, expected) in &recording.fixture.column_types {
        assert_eq!(
            read_column_types(&conn, table)?,
            *expected,
            "typeof over {table}"
        );
    }
    Ok(())
}

#[test]
fn opening_a_current_file_writes_nothing() -> TestResult<()> {
    let fixture = fixture_copy("fixture-v7.sqlite")?;
    migrate::sqlite(fixture.path())?;
    let before = fs::read(fixture.path())?;
    migrate::sqlite(fixture.path())?;
    let after = fs::read(fixture.path())?;
    assert_eq!(before, after, "a current file must open without rewrites");
    Ok(())
}

#[test]
fn a_delete_mode_v5_backup_is_the_original_file() -> TestResult<()> {
    let dir = tempdir()?;
    let path = delete_mode_v5(dir.path())?;
    let original = fs::read(&path)?;
    let status = migrate::sqlite(&path)?;
    assert_eq!(status.current, memory_sqlite::SCHEMA_VERSION);
    let backup = dir.path().join("memory.sqlite.v5.bak");
    assert_eq!(
        fs::read(&backup)?,
        original,
        "the .v5.bak must be the delete-mode original"
    );
    Ok(())
}

#[test]
fn an_existing_backup_is_not_replaced() -> TestResult<()> {
    let dir = tempdir()?;
    let path = delete_mode_v5(dir.path())?;
    let original = fs::read(&path)?;
    let backup = dir.path().join("memory.sqlite.v5.bak");
    let marker = b"pre-existing backup must stay untouched";
    fs::write(&backup, marker)?;
    match migrate::sqlite(&path) {
        Err(migrate::Error::BackupExists { path: reported }) => assert_eq!(reported, backup),
        other => panic!("expected BackupExists, got {other:?}"),
    }
    assert_eq!(
        fs::read(&path)?,
        original,
        "the database must stay unchanged"
    );
    assert_eq!(fs::read(&backup)?, marker, "the backup must stay unchanged");
    Ok(())
}

fn delete_mode_v5(dir: &std::path::Path) -> TestResult<std::path::PathBuf> {
    let path = dir.join("memory.sqlite");
    fs::copy(common::recording("upgrade-v5.sqlite"), &path)?;
    let conn = rusqlite::Connection::open(&path)?;
    let mode: String = conn.query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))?;
    assert_eq!(mode, "delete");
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    assert_eq!(version, 5);
    Ok(path)
}

#[test]
fn backup_symlinks_never_overwrite_their_targets() -> TestResult<()> {
    for target_exists in [false, true] {
        let dir = tempfile::tempdir()?;
        let path = delete_mode_v5(dir.path())?;
        let original = std::fs::read(&path)?;
        let target = dir.path().join("outside.sqlite");
        if target_exists {
            std::fs::write(&target, "keep this file")?;
        }
        let backup = dir.path().join("memory.sqlite.v5.bak");
        std::os::unix::fs::symlink(&target, &backup)?;
        assert!(
            matches!(migrate::sqlite(&path), Err(migrate::Error::BackupExists { path }) if path == backup)
        );
        assert_eq!(std::fs::read(&path)?, original);
        if target_exists {
            assert_eq!(std::fs::read_to_string(target)?, "keep this file");
        } else {
            assert!(!target.try_exists()?);
        }
    }
    Ok(())
}

#[test]
fn inspection_errors_do_not_become_an_empty_schema() -> TestResult<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("loop.sqlite");
    std::os::unix::fs::symlink(&path, &path)?;
    assert!(
        matches!(migrate::sqlite_status(&path), Err(migrate::Error::Inspect { path: failed, .. }) if failed == path)
    );
    assert!(
        matches!(migrate::sqlite(&path), Err(migrate::Error::Inspect { path: failed, .. }) if failed == path)
    );
    Ok(())
}
