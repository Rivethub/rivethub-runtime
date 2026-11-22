use rusqlite::Connection;

#[tokio::test]
async fn step_44_preserves_rows_and_converges_after_interruption()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let store = memory_sqlite::SqliteStore::open(&path)?;
    store
        .with_conn(|conn| {
            conn.execute_batch(include_str!("fixtures/pre_memory_transfer.txt"))
                .map_err(memory_sqlite::SqliteError::Pragma)?;
            conn.execute_batch(
                "DROP VIEW ros_summary_records_indexed;
                 DROP TRIGGER ros_summary_passages_invalidate;
                 DROP TABLE ros_summary_passages_fts;
                 DROP TABLE ros_summary_passages;",
            ).map_err(memory_sqlite::SqliteError::Pragma)?;
            conn.execute_batch(include_str!("../src/schema/summary_passages_v43.sql"))
                .map_err(memory_sqlite::SqliteError::Pragma)?;
            conn.execute_batch(
                "INSERT INTO ros_wiki_topics (slug, title, created_at, updated_at)
                 VALUES ('page', 'Page', '2026-01-01', '2026-01-01');
                 INSERT INTO rh_topic_pages (page_slug) VALUES ('page');
                 INSERT INTO ros_wiki_redirects (from_slug, to_slug, created_at)
                 VALUES ('moved', 'page', '2026-01-01');
                 INSERT INTO ros_tag_taxonomy (key, value, created_at, updated_at)
                 VALUES ('topic', 'page', '2026-01-01', '2026-01-01');
                 INSERT INTO rh_topic_tombstones (kind, slug, deleted_at)
                 VALUES ('work', 'gone', '2026-01-01');
                 INSERT INTO ros_summaries (id, kind, content, created_at)
                 VALUES ('kept', 'leaf', 'Kept', '2026-01-01T00:00:00Z');
                 INSERT INTO ros_summary_passages
                    (id, record_id, ordinal, char_start, char_end, content, earliest_at, latest_at, index_key)
                 VALUES ('passage', 'kept', 0, 0, 4, 'Kept', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', zeroblob(32));
                 PRAGMA user_version = 43;",
            )
            .map_err(memory_sqlite::SqliteError::Pragma)
        })
        .await?;
    drop(store);
    let pending = migrate::sqlite_status(&path)?;
    assert_eq!(
        (pending.current, pending.required, pending.pending),
        (43, 44, 1)
    );
    let upgraded = migrate::sqlite(&path)?;
    assert_eq!((upgraded.current, upgraded.pending), (44, 0));
    let conn = Connection::open(&path)?;
    let passage: String = conn.query_row(
        "SELECT content FROM ros_summary_passages WHERE id = 'passage'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(passage, "Kept");
    assert_altered_rows(&conn)?;
    conn.pragma_update(None, "user_version", 43)?;
    drop(conn);
    let backup = dir.path().join("memory.sqlite.v43.bak");
    assert!(backup.try_exists()?);
    std::fs::rename(backup, dir.path().join("retained.sqlite"))?;
    assert_eq!(migrate::sqlite(&path)?.current, 44);
    let upgraded = Connection::open(&path)?;
    assert_altered_rows(&upgraded)?;
    let fresh = dir.path().join("fresh.sqlite");
    migrate::sqlite(&fresh)?;
    assert_eq!(
        definitions(&upgraded)?,
        definitions(&Connection::open(fresh)?)?
    );
    Ok(())
}

fn assert_altered_rows(conn: &Connection) -> rusqlite::Result<()> {
    let count: i64 = conn.query_row(
        "SELECT (SELECT count(*) FROM ros_wiki_topics WHERE slug = 'page' AND visibility = 'private')
         + (SELECT count(*) FROM rh_topic_pages WHERE page_slug = 'page' AND sections_were_null = 0)
         + (SELECT count(*) FROM ros_wiki_redirects WHERE from_slug = 'moved' AND owner_user_id IS NULL)
         + (SELECT count(*) FROM ros_tag_taxonomy WHERE value = 'page' AND globally_shared IS NULL)
         + (SELECT count(*) FROM rh_topic_tombstones WHERE slug = 'gone' AND locally_deleted = 0)",
        [], |row| row.get(0),
    )?;
    assert_eq!(count, 5);
    Ok(())
}

fn definitions(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    conn.prepare("SELECT name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}
