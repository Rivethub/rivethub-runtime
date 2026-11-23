type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn top_down_upgrade_from_sqlite_35_preserves_legacy_kinds_and_matches_fresh() -> TestResult {
    use rusqlite::Connection;

    let dir = tempfile::tempdir()?;
    let fresh = dir.path().join("fresh.sqlite");
    let upgraded = dir.path().join("upgraded.sqlite");
    migrate::sqlite(&fresh)?;
    let conn = Connection::open(&fresh)?;
    conn.execute("VACUUM INTO ?1", [upgraded.to_string_lossy().as_ref()])?;
    let old = Connection::open(&upgraded)?;
    drop_summary_passage_schema(&old)?;
    old.execute_batch(
        "DROP VIEW rh_transcript_times;
         DROP INDEX rh_transcript_seek; DROP INDEX rh_transcript_recent;
         DROP TABLE IF EXISTS rh_summary_capture_clock;
         DROP TABLE rh_summary_checkpoints; DROP TABLE rh_summary_runs;
         DROP TABLE rh_summary_state; DROP TRIGGER rh_disarm_trashed_openings; DROP TABLE rh_summary_openings;
         ALTER TABLE ros_conversations DROP COLUMN title_from_capture;
         DROP TABLE rh_summary_conversion; DROP TABLE rh_summary_conversion_items;
         DROP TABLE ros_summary_deliveries;
         DROP INDEX rh_summary_one_session; DROP INDEX rh_summary_sources_order;
         ALTER TABLE ros_summary_sources DROP COLUMN render_key;
         ALTER TABLE ros_summaries DROP COLUMN summary_ordinal;
         ALTER TABLE ros_summaries DROP COLUMN index_needs_repair;
         ALTER TABLE ros_summaries DROP COLUMN issued_part_ordinal;
         ALTER TABLE ros_summaries DROP COLUMN input_token_estimate;
         ALTER TABLE ros_summaries DROP COLUMN source_first;
         ALTER TABLE ros_summaries DROP COLUMN source_last;
         ALTER TABLE ros_summaries DROP COLUMN range_verified;
         ALTER TABLE ros_summaries DROP COLUMN input_key;
         ALTER TABLE ros_summaries DROP COLUMN index_key;
         ALTER TABLE ros_summaries DROP COLUMN quality_flags;
         ALTER TABLE ros_summaries DROP COLUMN section_char_start;
         ALTER TABLE ros_summaries DROP COLUMN section_char_end;
         INSERT INTO ros_summaries (id, kind, depth, content, created_at)
           VALUES ('legacy', 'branch', 2, 'Old fixture note', '2026-10-10T00:00:00.000Z');
         INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at)
         VALUES ('kept','cli:kept','fixture','2026-10-10T00:00:00Z','2026-10-10T00:00:00Z'), ('trash','cli:trashed','fixture','2026-10-10T00:00:00Z','2026-10-10T00:00:00Z');
         INSERT INTO ros_summaries (id, conversation_id, kind, depth, content, created_at)
         VALUES ('legacy-kept','kept','leaf',0,'Invented packing review','2026-10-10'),
                ('legacy-trash','trash','leaf',0,'Invented list of fasteners','2026-10-10');
         INSERT INTO rh_session_trash (session_key, trashed_at) VALUES ('cli:trashed','2026-10-11');
         PRAGMA user_version = 35;",
    )?;
    drop(old);
    let pending = migrate::sqlite_status(&upgraded)?;
    let required = memory_sqlite::SCHEMA_VERSION;
    assert_eq!(
        (pending.current, pending.required, pending.pending),
        (35, required, usize::try_from(required - 35)?)
    );
    let status = migrate::sqlite(&upgraded)?;
    assert_eq!(
        (status.current, status.required, status.pending),
        (required, required, 0)
    );
    let up = Connection::open(&upgraded)?;
    let row: (String, i32, Option<i32>, bool) = up.query_row(
        "SELECT kind, depth, summary_ordinal, index_needs_repair FROM ros_summaries WHERE id = 'legacy'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let opening: (String, bool, bool, bool) = up.query_row(
        "SELECT conversation_id, filing_armed, title_armed, historical_pending FROM rh_summary_openings",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!(opening, ("kept".to_owned(), false, false, true));
    up.execute(
        "INSERT INTO rh_summary_conversion_items (conversation_id, last_input_at, estimated_tokens, state)
         VALUES ('kept','2026-10-11T00:00:00Z',1,'pending')",
        [],
    )?;
    let next: String = up.query_row(
        "SELECT next_at FROM rh_summary_conversion_items",
        [],
        |row| row.get(0),
    )?;
    assert!(next.parse::<jiff::Timestamp>().is_ok());
    assert_eq!(row, ("branch".to_owned(), 2, None, false));
    assert_eq!(sqlite_definitions(&conn)?, sqlite_definitions(&up)?);
    Ok(())
}

#[test]
fn summary_passage_upgrade_marks_pooled_indexes_for_repair() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let old = rusqlite::Connection::open(&path)?;
    drop_summary_passage_schema(&old)?;
    old.execute_batch(
        "INSERT INTO ros_summaries (id, kind, content, created_at, embedding, index_key)
         VALUES ('modern', 'session', 'Paired records', '2026-01-01T00:00:00Z', zeroblob(4096), zeroblob(32)),
                ('legacy', 'leaf', 'Older record', '2026-01-01T00:00:00Z', zeroblob(4096), NULL);
         PRAGMA user_version = 42;",
    )?;
    drop(old);
    migrate::sqlite(&path)?;
    let current = rusqlite::Connection::open(&path)?;
    let mut statement = current.prepare(
        "SELECT id, embedding IS NOT NULL, index_needs_repair FROM ros_summaries ORDER BY id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        rows,
        [
            ("legacy".to_owned(), true, false),
            ("modern".to_owned(), false, true)
        ]
    );
    Ok(())
}

fn drop_summary_passage_schema(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(include_str!("fixtures/pre_memory_transfer.txt"))?;
    conn.execute_batch(
        "DROP VIEW ros_summary_records_indexed;
         DROP TRIGGER ros_summary_passages_invalidate;
         DROP TABLE ros_summary_passages_fts;
         DROP TABLE ros_summary_passages;",
    )
}

fn sqlite_definitions(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<(String, String)>> {
    conn.prepare("SELECT name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}
