use rusqlite::Connection;

type Outcome = Result<(), Box<dyn std::error::Error>>;

#[test]
fn version_41_gains_entity_storage_and_preserves_existing_topics() -> Outcome {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let conn = Connection::open(&path)?;
    conn.execute_batch(
        "DROP TABLE rh_entity_extractions;
         DROP TABLE rh_entity_candidates;
         DROP TABLE rh_entity_decisions;
         DROP TABLE rh_entity_embeddings;
         DROP TABLE rh_entity_history;
         DROP TABLE rh_entity_applied;
         DROP TABLE rh_entity_type_seen;
         ALTER TABLE rh_entity_fact_claims DROP COLUMN settled;
         DELETE FROM rh_topics WHERE kind = 'type' AND slug IN ('organizations', 'projects', 'places');
         UPDATE rh_topics SET name = 'My hosts' WHERE kind = 'type' AND slug = 'hosts';
         PRAGMA user_version = 41;",
    )?;
    drop(conn);
    let upgraded = migrate::sqlite(&path)?;
    assert_eq!(upgraded.current, 42);
    let conn = Connection::open(&path)?;
    let tables: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name IN (
            'rh_entity_extractions', 'rh_entity_candidates', 'rh_entity_decisions',
            'rh_entity_embeddings', 'rh_entity_history', 'rh_entity_applied', 'rh_entity_type_seen'
         )",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(tables, 7);
    let hosts: String = conn.query_row(
        "SELECT name FROM rh_topics WHERE kind = 'type' AND slug = 'hosts'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(hosts, "My hosts");
    let seeds: i64 = conn.query_row(
        "SELECT count(*) FROM rh_topics WHERE kind = 'type' AND slug IN ('organizations', 'projects', 'places')",
        [], |row| row.get(0),
    )?;
    assert_eq!(seeds, 3);
    let default: String = conn.query_row(
        "SELECT dflt_value FROM pragma_table_info('rh_entity_fact_claims') WHERE name = 'settled'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(default, "0");
    assert!(
        directory
            .path()
            .join("memory.sqlite.v41.bak")
            .try_exists()?
    );
    Ok(())
}
