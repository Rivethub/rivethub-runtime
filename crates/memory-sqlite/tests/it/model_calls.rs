use memory_sqlite::SqliteStore;
use rusqlite::Connection;
use tempfile::TempDir;
use testkit::model_calls;

use crate::common::TestResult;

fn store() -> TestResult<(TempDir, SqliteStore)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let store = SqliteStore::open(&path)?;
    Ok((directory, store))
}

#[tokio::test]
async fn a_repeated_call_is_a_duplicate_and_changes_nothing() -> TestResult {
    let (_directory, store) = store()?;
    model_calls::a_repeated_call_is_a_duplicate_and_changes_nothing(&store).await?;
    Ok(())
}

#[tokio::test]
async fn a_batch_that_repeats_its_own_call_counts_the_second_as_duplicate() -> TestResult {
    let (_directory, store) = store()?;
    model_calls::a_batch_that_repeats_its_own_call_counts_the_second_as_duplicate(&store).await?;
    Ok(())
}

#[tokio::test]
async fn a_session_reads_its_own_calls_in_start_order_with_every_field() -> TestResult {
    let (directory, store) = store()?;
    model_calls::a_session_reads_its_own_calls_in_start_order_with_every_field(&store).await?;
    let conn = Connection::open(directory.path().join("memory.sqlite"))?;
    let unclaimed: i64 = conn.query_row(
        "SELECT count(*) FROM rh_model_calls WHERE session_key IS NULL",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(unclaimed, 1);
    Ok(())
}

#[tokio::test]
async fn deleting_a_session_deletes_its_model_calls() -> TestResult {
    let (_directory, store) = store()?;
    model_calls::deleting_a_session_deletes_its_model_calls(&store).await?;
    Ok(())
}
