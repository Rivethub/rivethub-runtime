#[expect(
    dead_code,
    reason = "the harness module is included whole by every test binary and this one uses a subset"
)]
mod common;

use common::TestResult;
use common::connection;
use common::fixture_copy;
use common::instant;
use common::read_column_types;
use common::read_fts_rows;
use common::read_legacy_master_entries;
use common::read_pragmas;
use common::read_rows;
use common::read_table_columns;
use common::replay_search_access;
use common::upgrade_recording;
use memory_sqlite::SqliteError;
use memory_sqlite::SqliteStore;
use tempfile::tempdir;

#[test]
fn recorded_upgrade_files_reach_the_migrated_state() -> TestResult<()> {
    let recording = upgrade_recording()?;
    assert_eq!(recording.cases.len(), 3, "the recorded upgrade inputs");
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
                        "schema v28 gives an unowned {table} row the registry owner in {}",
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
fn a_newer_schema_version_is_a_typed_error() -> TestResult<()> {
    let dir = tempdir()?;
    let path = dir.path().join("future.sqlite");
    {
        let conn = connection(&path)?;
        conn.execute_batch("PRAGMA user_version = 99;")?;
    }
    match SqliteStore::open(&path) {
        Err(SqliteError::UnsupportedSchema { found }) => assert_eq!(found, 99),
        other => panic!("expected UnsupportedSchema, got {other:?}"),
    }
    Ok(())
}

#[test]
fn an_unknown_schema_version_is_a_typed_error() -> TestResult<()> {
    let dir = tempdir()?;
    let path = dir.path().join("negative.sqlite");
    {
        let conn = connection(&path)?;
        conn.execute_batch("PRAGMA user_version = -2;")?;
    }
    match SqliteStore::open(&path) {
        Err(SqliteError::UnsupportedSchema { found }) => assert_eq!(found, -2),
        other => panic!("expected UnsupportedSchema, got {other:?}"),
    }
    Ok(())
}

#[test]
fn a_failed_open_reports_the_path() -> TestResult<()> {
    let dir = tempdir()?;
    let path = dir.path().join("directory.sqlite");
    std::fs::create_dir(&path)?;
    match SqliteStore::open(&path) {
        Err(SqliteError::Open { path: reported, .. }) => assert_eq!(reported, path),
        other => panic!("expected Open, got {other:?}"),
    }
    Ok(())
}

#[test]
fn schema_25_archives_the_tasks_the_adapter_kept() -> TestResult<()> {
    let directory = tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    {
        let conn = rusqlite::Connection::open(&path)?;
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
    assert_eq!(version, migrate::SchemaVersion::V31.number());
    let page_pk: i64 = conn.query_row(
        "SELECT pk FROM pragma_table_info('rh_topic_pages') WHERE name = 'page_slug'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(page_pk, 1);
    let archived: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'rh_tasks_archive'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(archived, 1);
    let trigger: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'trigger' AND name = 'messages_ad'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(trigger, 1);
    let page_owner: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('rh_topic_pages') WHERE name = 'owner_user_id'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(page_owner, 1);
    let task_owner: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('rh_tasks_archive') WHERE name = 'owner_user_id'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(task_owner, 1);
    Ok(())
}
