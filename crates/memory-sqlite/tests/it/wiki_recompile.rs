use memory_sqlite::SqliteStore;
use testkit::wiki_files::CONVERSATION;

use crate::common::TestResult;

#[tokio::test]
async fn recompile_restores_pushed_out_history_and_archives_copies() -> TestResult {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("memory.sqlite");
    migrate::sqlite(&database)?;
    rusqlite::Connection::open(&database)?.execute(
        "INSERT INTO ros_conversations
            (id, session_key, agent, title, created_at, updated_at, owner_user_id)
         VALUES (?1, 'claude-code:recompile', 'rivet', 'Recompile',
                 '2026-10-29T12:00:00Z', '2026-10-29T12:00:00Z', 'owner')",
        [CONVERSATION],
    )?;
    let root = std::fs::canonicalize(directory.path())?;
    let store = SqliteStore::open(&database)?.with_wiki_directory(root.clone());
    testkit::wiki_recompile::restores_history_and_archives_copies(&store, &root).await?;
    testkit::wiki_recompile::validates_cited_summaries(&store, &root).await
}
