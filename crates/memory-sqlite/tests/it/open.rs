use std::fs;
use std::path::Path;

use memory_sqlite::SCHEMA_VERSION;
use memory_sqlite::SqliteError;
use memory_sqlite::SqliteStore;
use sha2::Digest;
use sha2::Sha256;
use tempfile::tempdir;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture(name: &str) -> std::path::PathBuf {
    testkit::recording(&format!("memory-sqlite/{name}"))
}

#[test]
fn a_missing_file_is_not_initialised_and_is_never_created() -> TestResult {
    assert_eq!(SCHEMA_VERSION, 32);
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
fn a_behind_file_is_schema_behind_and_left_untouched() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("memory.sqlite");
    fs::copy(fixture("upgrade-v5.sqlite"), &path)?;
    let before = fs::read(&path)?;
    match SqliteStore::open(&path) {
        Err(SqliteError::SchemaBehind {
            path: reported,
            found,
            required,
        }) => {
            assert_eq!(reported, path);
            assert_eq!(found, 5);
            assert_eq!(required, SCHEMA_VERSION);
        }
        other => panic!("expected SchemaBehind, got {other:?}"),
    }
    assert_eq!(before, fs::read(&path)?, "open must not change the file");
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

fn refused_delete_mode(user_version: i64) -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("memory.sqlite");
    write_delete_mode(&path, user_version)?;
    let before = fs::read(&path)?;
    let before_hash = sha256(&before);
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
        (99, SqliteError::UnsupportedSchema { found }) => assert_eq!(*found, 99),
        (version, other) => {
            return Err(format!("user_version {version} produced {other:?}").into());
        }
    }
    let after = fs::read(&path)?;
    assert_eq!(sha256(&after), before_hash, "sha256 must match");
    assert_eq!(after, before, "the file bytes must match");
    let conn =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    assert_eq!(mode, "delete");
    Ok(())
}

fn write_delete_mode(path: &Path, user_version: i64) -> TestResult {
    let conn = rusqlite::Connection::open(path)?;
    conn.pragma_update(None, "user_version", user_version)?;
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    assert_eq!(mode, "delete", "the fixture must start in delete mode");
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
