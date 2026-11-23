use kernel::cleanup::CleanupSource as _;
use kernel::owner::Owner;
use kernel::queue::pause::PausableRole;
use kernel::queue::pause::WorkerPauses as _;
use uuid::Uuid;
use wire::message::Role;

use crate::common::RECORDED_START;
use crate::common::TestResult;
use crate::common::batch;
use crate::common::fresh_store;
use crate::common::instant;
use crate::common::message;
use crate::common::seed;

const OPENING: &str = "Repair the recovery callback";

#[tokio::test]
async fn cleanup_reads_the_opening_and_releases_the_read_only_pragma() -> TestResult {
    let fresh = fresh_store()?;
    let owner = Owner::new("dana".to_owned())?;
    let mut request = batch("claude-code:opening", "rivet", "claude-code");
    request.messages.push(message("e1", Role::User, OPENING));
    let captured = fresh
        .store
        .capture(request, &owner, None, instant(RECORDED_START)?)
        .await?;
    let id = Uuid::parse_str(&captured.conversation_id)?;
    testkit::maintenance::cleanup_reads_the_opening(&fresh.store, id, OPENING).await?;
    fresh.store.pause(PausableRole::Categorizer, "cli").await?;
    let roles: Vec<_> = fresh
        .store
        .paused()
        .await?
        .into_iter()
        .map(|pause| pause.role)
        .collect();
    assert_eq!(roles, [PausableRole::Categorizer]);
    Ok(())
}

#[tokio::test]
async fn an_eval_task_is_automated() -> TestResult {
    let fresh = fresh_store()?;
    let task = Uuid::parse_str("00000000-0000-4000-8000-0000000000e1")?;
    fresh
        .store
        .with_conn({
            let id = task.to_string();
            move |conn| {
                conn.execute(
                    "INSERT INTO rh_tasks_archive (id, origin) VALUES (?1, 'eval')",
                    [id],
                )
                .map_err(seed)?;
                Ok(())
            }
        })
        .await?;
    let owner = Owner::new("dana".to_owned())?;
    let mut request = batch(&format!("task:{task}"), "rivet", "task");
    request
        .messages
        .push(message("e1", Role::User, "Score the page"));
    let captured = fresh
        .store
        .capture(request, &owner, None, instant(RECORDED_START)?)
        .await?;
    let id = Uuid::parse_str(&captured.conversation_id)?;
    testkit::maintenance::an_eval_task_is_automated(&fresh.store, id).await
}

#[tokio::test]
async fn a_read_only_open_leaves_delete_journal_mode() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    drop(memory_sqlite::SqliteStore::open(&path)?);
    {
        let conn = rusqlite::Connection::open(&path)?;
        conn.pragma_update(None, "journal_mode", "DELETE")?;
    }
    let store = memory_sqlite::SqliteStore::open_read_only(&path)?;
    store.snapshot().await?;
    drop(store);
    let conn = rusqlite::Connection::open(&path)?;
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    assert_eq!(mode, "delete");
    Ok(())
}
