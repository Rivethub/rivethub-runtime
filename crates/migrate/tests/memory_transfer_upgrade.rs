use rusqlite::Connection;

#[tokio::test]
async fn version_43_preserves_passages_and_steps_directly_to_49()
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
                "INSERT INTO ros_summaries (id, kind, content, created_at)
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
    assert!(dir.path().join("memory.sqlite.v43.bak").try_exists()?);
    Ok(())
}
