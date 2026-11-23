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
