mod common;

use std::fs;

use common::TestResult;
use common::connection;
use common::fixture_copy;
use common::instant;
use common::read_column_types;
use common::read_fts_rows;
use common::read_legacy_master_entries;
use common::read_pragmas;
use common::read_row_count;
use common::read_rows;
use common::read_table_columns;
use common::replay_search_access;
use common::schema_recording;
use common::upgrade_recording;
use tempfile::tempdir;

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
fn an_upgraded_file_keeps_the_legacy_schema_and_rows() -> TestResult<()> {
    let recording = schema_recording()?;
    let fixture = fixture_copy("fixture-v7.sqlite")?;
    migrate::sqlite(fixture.path())?;
    let conn = connection(fixture.path())?;
    common::assert_upgraded_pragmas(&read_pragmas(&conn)?, &recording.fixture.pragmas);
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
fn the_backup_of_a_delete_mode_file_is_the_original() -> TestResult<()> {
    let dir = tempdir()?;
    let path = delete_mode(dir.path())?;
    let original = fs::read(&path)?;
    let status = migrate::sqlite(&path)?;
    assert_eq!(status.current, memory_sqlite::SCHEMA_VERSION);
    let backup = dir.path().join("memory.sqlite.v5.bak");
    assert_eq!(
        fs::read(&backup)?,
        original,
        "the backup must be the delete-mode original"
    );
    Ok(())
}

#[test]
fn an_existing_backup_is_not_replaced() -> TestResult<()> {
    let dir = tempdir()?;
    let path = delete_mode(dir.path())?;
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

fn delete_mode(dir: &std::path::Path) -> TestResult<std::path::PathBuf> {
    let path = dir.join("memory.sqlite");
    fs::copy(common::recording("upgrade-v5.sqlite"), &path)?;
    let conn = rusqlite::Connection::open(&path)?;
    conn.query_row("PRAGMA journal_mode = DELETE", [], |row| {
        row.get::<_, String>(0)
    })?;
    Ok(path)
}

#[test]
fn backup_symlinks_never_overwrite_their_targets() -> TestResult<()> {
    for target_exists in [false, true] {
        let dir = tempdir()?;
        let path = delete_mode(dir.path())?;
        let original = fs::read(&path)?;
        let target = dir.path().join("outside.sqlite");
        if target_exists {
            fs::write(&target, "keep this file")?;
        }
        let backup = dir.path().join("memory.sqlite.v5.bak");
        std::os::unix::fs::symlink(&target, &backup)?;
        assert!(
            matches!(migrate::sqlite(&path), Err(migrate::Error::BackupExists { path }) if path == backup)
        );
        assert_eq!(fs::read(&path)?, original);
        if target_exists {
            assert_eq!(fs::read_to_string(target)?, "keep this file");
        } else {
            assert!(!target.try_exists()?);
        }
    }
    Ok(())
}

#[test]
fn inspection_errors_do_not_become_an_empty_schema() -> TestResult<()> {
    let dir = tempdir()?;
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

#[test]
fn recorded_upgrade_files_reach_the_migrated_state() -> TestResult<()> {
    let recording = upgrade_recording()?;
    for case in &recording.cases {
        let fixture = fixture_copy(&case.input_file)?;
        let migrated = &case.migrated;
        migrate::sqlite_at(fixture.path(), instant(&migrated.clock)?)?;
        let conn = connection(fixture.path())?;
        replay_search_access(&conn, migrated)?;
        common::assert_upgraded_pragmas(&read_pragmas(&conn)?, &migrated.pragmas);
        assert_eq!(
            read_legacy_master_entries(&conn)?,
            migrated.sqlite_master,
            "sqlite_master after opening {}",
            case.input_file
        );
        for (table, expected) in &migrated.table_info {
            assert_eq!(
                read_table_columns(&conn, table)?,
                *expected,
                "table_info of {table} in {}",
                case.input_file
            );
        }
        for (table, expected) in &migrated.tables {
            let mut actual = read_rows(&conn, table)?;
            if table == "ros_conversations" || table == "ros_messages" {
                for row in &mut actual {
                    assert_eq!(
                        row.insert("owner_user_id".to_owned(), common::Cell::Null),
                        Some(common::Cell::Text("owner".to_owned())),
                        "an unowned {table} row takes the registry owner in {}",
                        case.input_file
                    );
                }
            }
            assert_eq!(actual, *expected, "rows of {table} in {}", case.input_file);
        }
        for (name, expected) in &migrated.fts {
            assert_eq!(
                read_fts_rows(&conn, name)?,
                *expected,
                "fts rows of {name} in {}",
                case.input_file
            );
        }
        for (table, expected) in &migrated.column_types {
            let mut actual = read_column_types(&conn, table)?;
            if (table == "ros_conversations" || table == "ros_messages")
                && let Some(owners) = actual.get_mut("owner_user_id")
                && let Some(owned) = owners.remove("text")
            {
                owners.insert("null".to_owned(), owned);
            }
            assert_eq!(
                actual, *expected,
                "typeof over {table} in {}",
                case.input_file
            );
        }
    }
    Ok(())
}

#[test]
fn old_tasks_are_archived_and_the_table_is_rebuilt() -> TestResult<()> {
    let directory = tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    {
        let conn = rusqlite::Connection::open(&path)?;
        conn.execute_batch(include_str!("fixtures/pre_memory_transfer.txt"))?;
        conn.execute_batch(
            "DROP TABLE rh_tasks;
             CREATE TABLE rh_tasks (id TEXT PRIMARY KEY, goal TEXT NOT NULL, executor TEXT NOT NULL,
                 status TEXT NOT NULL, error TEXT, completed_at TEXT);
             INSERT INTO rh_tasks VALUES ('a', 'beat', 'chat-loop', 'queued', NULL, NULL),
                 ('b', 'done', 'harness-session', 'completed', NULL, '2026-10-01T00:00:00.000Z');
             PRAGMA user_version = 24;",
        )?;
    }
    migrate::sqlite(&path)?;
    let conn = rusqlite::Connection::open(&path)?;
    let mut statement =
        conn.prepare("SELECT id, status, error FROM rh_tasks_archive ORDER BY id")?;
    let archived = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        archived,
        vec![
            (
                "a".to_owned(),
                "killed".to_owned(),
                Some("archived with the tasks refactor".to_owned())
            ),
            ("b".to_owned(), "completed".to_owned(), None),
        ]
    );
    let fresh: i64 = conn.query_row("SELECT count(*) FROM rh_tasks", [], |row| row.get(0))?;
    assert_eq!(fresh, 0);
    let shaped: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('rh_tasks') WHERE name = 'child_session_id'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(shaped, 1);
    Ok(())
}

#[test]
fn a_trigger_for_an_unloaded_module_does_not_block_renames() -> TestResult<()> {
    let directory = tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    {
        let conn = rusqlite::Connection::open(&path)?;
        conn.execute_batch(include_str!("fixtures/pre_memory_transfer.txt"))?;
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TABLE rh_topic_pages;
             CREATE TABLE rh_topic_pages (
                 topic_id TEXT PRIMARY KEY,
                 page_slug TEXT NOT NULL UNIQUE,
                 sections TEXT NOT NULL DEFAULT '[]',
                 owner_user_id TEXT
             );
             ALTER TABLE rh_tasks ADD COLUMN executor TEXT;
             CREATE TABLE messages (id INTEGER PRIMARY KEY, body TEXT);
             PRAGMA writable_schema = ON;
             INSERT INTO sqlite_master (type, name, tbl_name, rootpage, sql)
             VALUES ('table', 'messages_vec', 'messages_vec', 0,
                     'CREATE VIRTUAL TABLE messages_vec USING vec0(id float[4])');
             INSERT INTO sqlite_master (type, name, tbl_name, rootpage, sql)
             VALUES ('trigger', 'messages_ad', 'messages', 0,
                     'CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
                        DELETE FROM messages_vec WHERE id = OLD.id;
                      END');
             PRAGMA writable_schema = OFF;
             PRAGMA user_version = 17;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )?;
    }
    migrate::sqlite(&path)?;
    let conn = rusqlite::Connection::open(&path)?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    assert_eq!(version, memory_sqlite::SCHEMA_VERSION);
    for (table, column) in [
        ("rh_topic_pages", "owner_user_id"),
        ("rh_tasks_archive", "owner_user_id"),
    ] {
        let present: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info(?1) WHERE name = ?2",
            [table, column],
            |row| row.get(0),
        )?;
        assert_eq!(present, 1, "{table}.{column}");
    }
    let page_pk: i64 = conn.query_row(
        "SELECT pk FROM pragma_table_info('rh_topic_pages') WHERE name = 'page_slug'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(page_pk, 1);
    let trigger: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'trigger' AND name = 'messages_ad'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(trigger, 1);
    Ok(())
}
