use kernel::memory::MemoryState;
use kernel::memory::SessionCatalog;
use kernel::memory::SessionQuery;
use kernel::memory::SessionView;
use memory_sqlite::SqliteStore;
use rusqlite::Connection;
use tempfile::TempDir;
use testkit::session_catalog;
use testkit::session_imports;

use crate::common::TestResult;

fn store() -> TestResult<(TempDir, SqliteStore)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let store = SqliteStore::open(&path)?;
    Ok((directory, store))
}

fn connection(directory: &TempDir) -> TestResult<Connection> {
    Ok(Connection::open(directory.path().join("memory.sqlite"))?)
}

#[tokio::test]
async fn sessions_list_newest_first_with_counts_and_filters() -> TestResult {
    let (_directory, store) = store()?;
    session_catalog::lists_sessions_newest_first_with_counts_and_filters(&store, &store).await
}

#[tokio::test]
async fn keep_and_topics_shape_the_views() -> TestResult {
    let (_directory, store) = store()?;
    session_catalog::keep_and_topics_shape_the_views(&store, &store, &store, &store).await
}

#[tokio::test]
async fn trash_is_reversible_and_leaves_recall() -> TestResult {
    let (_directory, store) = store()?;
    session_catalog::trash_is_reversible_and_leaves_recall(&store, &store, &store).await
}

#[tokio::test]
async fn purge_removes_only_old_trash_through_deletion() -> TestResult {
    let (_directory, store) = store()?;
    session_catalog::purge_removes_only_old_trash_through_deletion(&store, &store, &store).await
}

#[tokio::test]
async fn children_are_excluded_and_titles_are_derived() -> TestResult {
    let (_directory, store) = store()?;
    session_catalog::excludes_children_and_derives_titles(&store, &store).await
}

#[tokio::test]
async fn memory_state_follows_summaries_and_flagged_jobs() -> TestResult {
    let (directory, store) = store()?;
    let seeded = session_catalog::seed(&store).await?;
    let conn = connection(&directory)?;
    let stamp = "2026-10-27T12:00:00.000Z";
    conn.execute(
        "INSERT INTO ros_summaries (id, conversation_id, content, created_at)
         SELECT 'sum-1', id, 'First line of the summary' || char(10) || 'second line', ?2
           FROM ros_conversations WHERE session_key = ?1",
        rusqlite::params![seeded.alpha.as_str(), stamp],
    )?;
    conn.execute(
        "INSERT INTO ros_jobs (id, task, job_key, payload, run_at, state, created_at, updated_at)
         SELECT 'job-dead', 'compact-conversation', 'k-dead',
                json_object('conversationId', id), ?2, 'dead', ?2, ?2
           FROM ros_conversations WHERE session_key = ?1",
        rusqlite::params![seeded.gamma.as_str(), stamp],
    )?;
    let page = store.list(&SessionQuery::new(SessionView::All)).await?;
    let state = |session: &wire::session_id::SessionId| {
        page.items
            .iter()
            .find(|item| item.id.as_str() == session.as_str())
            .map(|item| item.memory)
    };
    assert_eq!(state(&seeded.alpha), Some(MemoryState::Summarized));
    assert_eq!(state(&seeded.beta), Some(MemoryState::Capturing));
    assert_eq!(state(&seeded.gamma), Some(MemoryState::Flagged));
    assert_eq!(state(&seeded.delta), Some(MemoryState::Queued));
    let alpha = page
        .items
        .iter()
        .find(|item| item.id.as_str() == seeded.alpha.as_str())
        .ok_or("alpha")?;
    assert_eq!(
        alpha.gist.as_deref(),
        Some("First line of the summary second line")
    );
    Ok(())
}

#[tokio::test]
async fn bare_claude_sessions_list_under_their_harness() -> TestResult {
    let (_directory, store) = store()?;
    session_catalog::lists_bare_claude_sessions_under_their_harness(&store, &store).await
}

#[tokio::test]
async fn records_answer_bare_and_keyed_sessions_in_one_lookup() -> TestResult {
    let (_directory, store) = store()?;
    testkit::session_records::records_answer_bare_and_keyed_sessions_in_one_lookup(&store, &store)
        .await
}

#[tokio::test]
async fn an_import_lists_with_its_origin() -> TestResult {
    let (_directory, store) = store()?;
    session_imports::an_import_lists_with_its_origin(&store, &store).await
}

#[tokio::test]
async fn a_harness_filter_and_the_lookups_leave_imports_to_their_own_key() -> TestResult {
    let (_directory, store) = store()?;
    session_imports::a_harness_filter_and_the_session_lookup_leave_imports_to_their_own_key(
        &store, &store,
    )
    .await
}

#[tokio::test]
async fn an_import_moves_between_the_views_and_deletes() -> TestResult {
    let (_directory, store) = store()?;
    session_imports::an_import_moves_between_the_views_and_deletes(&store, &store, &store).await
}

#[tokio::test]
async fn paging_walks_across_an_import() -> TestResult {
    let (_directory, store) = store()?;
    session_imports::paging_walks_across_an_import(&store, &store).await
}

#[tokio::test]
async fn an_import_is_a_finished_record_ended_when_its_origin_ended() -> TestResult {
    let (_directory, store) = store()?;
    session_imports::an_import_is_a_finished_record_ended_when_its_origin_ended(&store, &store)
        .await
}
