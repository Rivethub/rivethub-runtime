use std::fs;
use std::path::Path;

use memory_sqlite::SCHEMA_VERSION;
use memory_sqlite::SqliteError;
use memory_sqlite::SqliteStore;
use tempfile::tempdir;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn a_missing_file_is_not_initialised_and_is_never_created() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("memory.sqlite");
    match SqliteStore::open(&path) {
        Err(SqliteError::NotInitialised { path: reported }) => assert_eq!(reported, path),
        other => panic!("expected NotInitialised, got {other:?}"),
    }
    assert!(!path.exists(), "open must not create the file");
    Ok(())
}

#[test]
fn a_delete_mode_uninitialised_file_is_byte_identical() -> TestResult {
    refused_delete_mode(0)
}

#[test]
fn a_delete_mode_behind_file_is_byte_identical() -> TestResult {
    refused_delete_mode(5)
}

#[test]
fn a_delete_mode_unsupported_file_is_byte_identical() -> TestResult {
    refused_delete_mode(99)
}

#[test]
fn a_delete_mode_negative_version_file_is_byte_identical() -> TestResult {
    refused_delete_mode(-2)
}

#[test]
fn a_failed_open_reports_the_path() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("directory.sqlite");
    fs::create_dir(&path)?;
    match SqliteStore::open(&path) {
        Err(SqliteError::Open { path: reported, .. }) => assert_eq!(reported, path),
        other => panic!("expected Open, got {other:?}"),
    }
    Ok(())
}

fn refused_delete_mode(user_version: i64) -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("memory.sqlite");
    write_delete_mode(&path, user_version)?;
    let before = fs::read(&path)?;
    let Err(error) = SqliteStore::open(&path) else {
        return Err("a refused version must not open".into());
    };
    let message = error.to_string();
    if user_version == 0 || user_version == 5 {
        assert!(
            message.contains(&format!("rh db migrate --path {}", path.display())),
            "the hint must name the real path: {message}"
        );
        assert!(
            !message.contains("<file>"),
            "the hint must not keep the placeholder: {message}"
        );
    }
    match (user_version, &error) {
        (0, SqliteError::NotInitialised { path: reported }) => assert_eq!(reported, &path),
        (
            5,
            SqliteError::SchemaBehind {
                path: reported,
                found,
                required,
            },
        ) => {
            assert_eq!(reported, &path);
            assert_eq!(*found, 5);
            assert_eq!(*required, SCHEMA_VERSION);
        }
        (-2 | 99, SqliteError::UnsupportedSchema { found }) => assert_eq!(*found, user_version),
        (version, other) => {
            return Err(format!("user_version {version} produced {other:?}").into());
        }
    }
    assert_eq!(fs::read(&path)?, before, "the file bytes must match");
    let conn =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    assert_eq!(mode, "delete");
    Ok(())
}

fn write_delete_mode(path: &Path, user_version: i64) -> TestResult {
    let conn = rusqlite::Connection::open(path)?;
    conn.pragma_update(None, "user_version", user_version)?;
    Ok(())
}
