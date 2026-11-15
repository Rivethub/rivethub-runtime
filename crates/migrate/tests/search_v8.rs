use rusqlite::Connection;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn an_upgrade_backfills_the_trigram_indexes_and_keeps_the_original_backup() -> TestResult {
    let fixture = testkit::copy_fixture(&testkit::recording("memory-sqlite/fixture-v7.sqlite"))?;
    let original = std::fs::read(fixture.path())?;
    let status = migrate::sqlite(fixture.path())?;
    assert_eq!(status.current, memory_sqlite::SCHEMA_VERSION);
    let backup = fixture.path().with_extension("sqlite.v7.bak");
    assert_eq!(std::fs::read(backup)?, original);
    let conn = Connection::open(fixture.path())?;
    for (table, index) in [
        ("ros_messages", "rh_messages_trigram"),
        ("ros_summaries", "rh_summaries_trigram"),
    ] {
        let missing:u32=conn.query_row(&format!("SELECT count(*) FROM (SELECT id,content FROM {table} EXCEPT SELECT id,content FROM {index})"),[],|row|row.get(0))?;
        let extra:u32=conn.query_row(&format!("SELECT count(*) FROM (SELECT id,content FROM {index} EXCEPT SELECT id,content FROM {table})"),[],|row|row.get(0))?;
        assert_eq!((missing, extra), (0, 0));
        let matches: u32 = conn.query_row(
            &format!("SELECT count(*) FROM {index} WHERE {index} MATCH 'deploy'"),
            [],
            |row| row.get(0),
        )?;
        assert_ne!(matches, 0);
    }
    let missing_tools:u32=conn.query_row("SELECT count(*) FROM (SELECT id,tool_result FROM ros_messages EXCEPT SELECT id,tool_result FROM rh_messages_trigram)",[],|row|row.get(0))?;
    assert_eq!(missing_tools, 0);
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    assert_eq!(version, memory_sqlite::SCHEMA_VERSION);
    Ok(())
}
