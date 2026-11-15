use std::fmt::Write;
use std::path::Path;
use std::time::Duration;

use memory_sqlite::import::ImportTable;

use super::{TestResult, provider, start, stop, write_state};

fn configure(home: &Path, dump: Option<&Path>) -> TestResult<testkit::ports::ClaimedPort> {
    let mut memory = "memory:\n  postgres:\n    embedded: {}\n".to_owned();
    if let Some(dump) = dump {
        writeln!(memory, "  import_pg_dump: {}", dump.display())?;
    }
    let port = testkit::ports::claim()?;
    write!(memory, "gateway:\n  host: 127.0.0.1\n  port: {port}\n")?;
    write_state(home, &provider(home, &memory))?;
    Ok(port)
}

async fn failure(home: &Path) -> TestResult<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_rh"))
            .args(["local", "--no-lan"])
            .env_clear()
            .env("HOME", home)
            .env("TOKIO_WORKER_THREADS", "2")
            .current_dir(home)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    assert!(!output.status.success());
    let logs = String::from_utf8(output.stderr)?;
    assert!(!logs.contains("gateway listening on"), "{logs}");
    assert!(!logs.contains("worker running"), "{logs}");
    assert!(!home.join(".rivethub/rivethub.pid").exists());
    Ok(logs)
}

#[tokio::test]
async fn absent_import_configuration_and_file_refuse_before_serving() -> TestResult {
    let home = tempfile::tempdir()?;
    let _claim = configure(home.path(), None)?;
    let logs = failure(home.path()).await?;
    assert!(logs.contains("set memory.import_pg_dump"), "{logs}");
    assert!(
        logs.contains("pg_dump --data-only --format=plain"),
        "{logs}"
    );
    assert!(logs.contains("--table=public.ros_messages"), "{logs}");
    let missing = home.path().join("absent.pg.txt");
    let _claim = configure(home.path(), Some(&missing))?;
    let logs = failure(home.path()).await?;
    assert!(
        logs.contains(&format!("dump {} is absent", missing.display())),
        "{logs}"
    );
    assert!(
        logs.contains("pg_dump --data-only --format=plain"),
        "{logs}"
    );
    let store = memory_sqlite::SqliteStore::open(home.path().join(".rivethub/memory.sqlite"))?;
    assert!(store.pg_import_report().await?.is_none());
    Ok(())
}

#[tokio::test]
async fn recorded_dump_imports_before_serving_and_receipt_allows_restart_without_dump() -> TestResult
{
    let home = tempfile::tempdir()?;
    let dump = home.path().join("memory.pg.txt");
    std::fs::copy(
        testkit::recording("memory-sqlite/import/pre-0020.pg.txt"),
        &dump,
    )?;
    let _claim = configure(home.path(), Some(&dump))?;
    let started = start(
        home.path(),
        &["local", "--no-lan"],
        &["gateway listening on"],
    )
    .await?;
    let logs = stop(started).await?;
    let imported = logs
        .find("one-time PGlite import completed")
        .ok_or("missing import log")?;
    assert!(
        imported
            < logs
                .find("gateway listening on")
                .ok_or("missing listener log")?
    );
    let path = home.path().join(".rivethub/memory.sqlite");
    let store = memory_sqlite::SqliteStore::open(&path)?;
    let receipt = store.pg_import_report().await?.ok_or("missing receipt")?;
    assert_eq!(receipt.rows.get(&ImportTable::Messages), Some(&1));
    drop(store);
    std::fs::remove_file(dump)?;
    let _claim = configure(home.path(), None)?;
    let restarted = start(
        home.path(),
        &["local", "--no-lan"],
        &["gateway listening on"],
    )
    .await?;
    let logs = stop(restarted).await?;
    assert!(!logs.contains("one-time PGlite import completed"), "{logs}");
    let store = memory_sqlite::SqliteStore::open(path)?;
    assert_eq!(store.pg_import_report().await?, Some(receipt));
    Ok(())
}

#[tokio::test]
async fn failed_import_can_retry_through_the_same_local_command() -> TestResult {
    let home = tempfile::tempdir()?;
    let dump = home.path().join("memory.pg.txt");
    std::fs::write(&dump, "COPY public.ros_messages (id) FROM stdin;\n")?;
    let _claim = configure(home.path(), Some(&dump))?;
    let logs = failure(home.path()).await?;
    assert!(logs.contains("missing required column"), "{logs}");
    let path = home.path().join(".rivethub/memory.sqlite");
    let store = memory_sqlite::SqliteStore::open(&path)?;
    assert!(store.pg_import_report().await?.is_none());
    drop(store);
    std::fs::copy(
        testkit::recording("memory-sqlite/import/pre-0020.pg.txt"),
        dump,
    )?;
    let started = start(
        home.path(),
        &["local", "--no-lan"],
        &["gateway listening on"],
    )
    .await?;
    stop(started).await?;
    let store = memory_sqlite::SqliteStore::open(path)?;
    assert!(store.pg_import_report().await?.is_some());
    Ok(())
}

#[tokio::test]
async fn nonempty_target_without_receipt_refuses_before_serving() -> TestResult {
    let home = tempfile::tempdir()?;
    let _claim = configure(home.path(), None)?;
    failure(home.path()).await?;
    let path = home.path().join(".rivethub/memory.sqlite");
    let conn = rusqlite::Connection::open(&path)?;
    conn.execute("INSERT INTO ros_meta VALUES ('unrelated', 'existing')", [])?;
    let _claim = configure(
        home.path(),
        Some(&testkit::recording("memory-sqlite/import/pre-0020.pg.txt")),
    )?;
    let logs = failure(home.path()).await?;
    assert!(logs.contains("not empty"), "{logs}");
    let value: String = conn.query_row(
        "SELECT value FROM ros_meta WHERE key = 'unrelated'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(value, "existing");
    Ok(())
}
