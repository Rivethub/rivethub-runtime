use std::path::Path;

use memory_sqlite::SqliteStore;
use testkit::wiki_files::CONVERSATION;
use testkit::wiki_files::same_commits;

use crate::common::TestResult;

#[tokio::test]
async fn page_files_match_postgres() -> TestResult {
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("memory.sqlite");
    migrate::sqlite(&database)?;
    let wiki = dir.path().join("wiki");
    std::fs::create_dir(&wiki)?;
    seed(&database)?;
    let store = SqliteStore::open(&database)?.with_wiki_directory(wiki.clone());
    same_commits(&store, &wiki).await?;
    testkit::wiki_files::commit_failure_keeps_rows(&store, &wiki, || {
        wiki_files::wiki_writer::fail_git(&wiki, "commit")
    })
    .await?;
    testkit::wiki_files::stale_publish_keeps_the_file(&store, &wiki).await?;
    Ok(())
}

#[tokio::test]
async fn a_late_sweep_keeps_the_entity_page() -> TestResult {
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("memory.sqlite");
    migrate::sqlite(&database)?;
    let wiki = dir.path().join("wiki");
    std::fs::create_dir(&wiki)?;
    seed(&database)?;
    let store = SqliteStore::open(&database)?.with_wiki_directory(wiki.clone());
    testkit::wiki_files::a_late_sweep_keeps_the_entity_page(&store, &wiki).await?;
    Ok(())
}

#[tokio::test]
async fn a_numbered_long_slug_keeps_the_page_it_shortens_to() -> TestResult {
    let (_dir, wiki, store) = filed_store()?;
    testkit::wiki_files::a_numbered_long_slug_keeps_the_page_it_shortens_to(&store, &wiki).await?;
    Ok(())
}

#[tokio::test]
async fn a_sweep_skips_a_page_file_without_a_row() -> TestResult {
    let (_dir, wiki, store) = filed_store()?;
    testkit::wiki_files::a_sweep_skips_a_page_file_without_a_row(&store, &wiki).await?;
    Ok(())
}

#[tokio::test]
async fn a_sweep_keeps_a_page_its_pull_brings_in() -> TestResult {
    let (_dir, wiki, store) = filed_store()?;
    testkit::wiki_files::a_sweep_keeps_a_page_its_pull_brings_in(&store, &wiki).await?;
    Ok(())
}

fn filed_store() -> TestResult<(tempfile::TempDir, std::path::PathBuf, SqliteStore)> {
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("memory.sqlite");
    migrate::sqlite(&database)?;
    let wiki = dir.path().join("wiki");
    std::fs::create_dir(&wiki)?;
    let store = SqliteStore::open(&database)?.with_wiki_directory(wiki.clone());
    Ok((dir, wiki, store))
}

fn seed(database: &Path) -> TestResult {
    let conn = rusqlite::Connection::open(database)?;
    conn.execute(
        "INSERT INTO ros_conversations
            (id, session_key, agent, title, created_at, updated_at, owner_user_id)
         VALUES (?1, 'claude-code:page-files', 'rivet', 'page files', '2026-10-10T12:00:00.000Z',
                 '2026-10-10T12:00:00.000Z', 'owner')",
        [CONVERSATION],
    )?;
    Ok(())
}
