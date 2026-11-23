type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn wiki_topics_keep_history_and_omit_extra_sections() -> TestResult {
    let dir = tempfile::tempdir()?;
    let fresh = dir.path().join("fresh.sqlite");
    migrate::sqlite(&fresh)?;
    assert_topic_columns(&rusqlite::Connection::open(&fresh)?)?;

    let upgraded = dir.path().join("upgraded.sqlite");
    migrate::sqlite(&upgraded)?;
    let old = rusqlite::Connection::open(&upgraded)?;
    drop_added_topic_columns(&old)?;
    old.execute(
        "INSERT INTO ros_wiki_topics (slug, title, current_state, created_at, updated_at)
         VALUES ('retained', 'Existing wiki', 'Existing summary', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )?;
    old.pragma_update(None, "user_version", 46)?;
    drop(old);
    migrate::sqlite(&upgraded)?;
    let conn = rusqlite::Connection::open(&upgraded)?;
    assert_topic_columns(&conn)?;
    let page: (String, String) = conn.query_row(
        "SELECT current_state, history FROM ros_wiki_topics WHERE slug = 'retained'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(page, ("Existing summary".to_owned(), "[]".to_owned()));
    Ok(())
}

#[test]
fn upgraded_task_archive_lookups_use_an_index() -> TestResult {
    let dir = tempfile::tempdir()?;
    let fresh = dir.path().join("fresh.sqlite");
    let upgraded = dir.path().join("upgraded.sqlite");
    migrate::sqlite(&fresh)?;
    migrate::sqlite(&upgraded)?;
    let old = rusqlite::Connection::open(&upgraded)?;
    old.execute_batch(
        "DROP TABLE rh_worker_pauses;
         DROP TABLE rh_tasks_archive;
         CREATE TABLE rh_tasks_archive (id TEXT, goal TEXT);
         INSERT INTO rh_tasks_archive VALUES ('task-a', 'existing goal');
         ALTER TABLE ros_wiki_topics DROP COLUMN history;
         INSERT INTO ros_wiki_topics (slug, title, current_state, created_at, updated_at)
              VALUES ('retained', 'Existing wiki', 'Existing summary', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         PRAGMA user_version = 46;",
    )?;
    drop(old);
    let status = migrate::sqlite(&upgraded)?;
    assert_eq!(status.current, memory_sqlite::SCHEMA_VERSION);
    let upgraded = rusqlite::Connection::open(&upgraded)?;
    let page: (String, String) = upgraded.query_row(
        "SELECT current_state, history FROM ros_wiki_topics WHERE slug = 'retained'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(page, ("Existing summary".to_owned(), "[]".to_owned()));
    let task: (String, Option<String>) = upgraded.query_row(
        "SELECT goal, origin FROM rh_tasks_archive WHERE id = 'task-a'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(task, ("existing goal".to_owned(), None));
    let fresh = rusqlite::Connection::open(&fresh)?;
    assert_indexed(&lookup_plan(&fresh)?);
    assert_indexed(&lookup_plan(&upgraded)?);
    Ok(())
}

#[test]
fn maintenance_can_run_again_and_settles_the_tail_indexes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    for _ in 0..2 {
        let old = rusqlite::Connection::open(&path)?;
        old.execute_batch(
            "DROP INDEX idx_ros_messages_unembedded;
             DROP INDEX idx_ros_conversations_owner;
             DROP INDEX idx_ros_tags_removal_state;
             PRAGMA user_version = 46;",
        )?;
        drop(old);
        let status = migrate::sqlite(&path)?;
        let required = memory_sqlite::SCHEMA_VERSION;
        assert_eq!((status.current, status.pending), (required, 0));
        let upgraded = rusqlite::Connection::open(&path)?;
        assert_topic_columns(&upgraded)?;
        let indexes: i64 = upgraded.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name IN
                ('idx_ros_messages_unembedded', 'idx_ros_conversations_owner', 'idx_ros_tags_removal_state')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(indexes, 3);
        std::fs::remove_file(path.with_extension("sqlite.v46.bak"))?;
    }
    Ok(())
}

fn assert_indexed(plan: &str) {
    let indexed = plan
        .lines()
        .filter(|line| line.contains("USING INDEX"))
        .count();
    let scanned = plan.lines().filter(|line| line.contains("SCAN")).count();
    assert_eq!((indexed, scanned), (1, 0), "{plan}");
}

fn lookup_plan(conn: &rusqlite::Connection) -> Result<String, rusqlite::Error> {
    let mut statement =
        conn.prepare("EXPLAIN QUERY PLAN SELECT origin FROM rh_tasks_archive WHERE id = 'task-a'")?;
    let details = statement
        .query_map([], |row| row.get::<_, String>(3))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(details.join("\n"))
}

fn assert_topic_columns(conn: &rusqlite::Connection) -> TestResult {
    let names = column_names(conn, "ros_wiki_topics")?;
    let added: Vec<&str> = names
        .iter()
        .filter(|name| *name == "history" || *name == "extra_sections")
        .map(String::as_str)
        .collect();
    assert_eq!(added, ["history"]);
    Ok(())
}

fn drop_added_topic_columns(conn: &rusqlite::Connection) -> Result<(), rusqlite::Error> {
    drop_column(conn, "ros_wiki_topics", "history")?;
    drop_column(conn, "ros_wiki_topics", "extra_sections")
}

fn drop_column(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> Result<(), rusqlite::Error> {
    if column_present(conn, table, column)? {
        conn.execute_batch(&format!("ALTER TABLE {table} DROP COLUMN {column}"))?;
    }
    Ok(())
}

fn column_present(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> Result<bool, rusqlite::Error> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info(?1) WHERE name = ?2",
        [table, column],
        |row| row.get(0),
    )?;
    Ok(count == 1)
}

fn column_names(conn: &rusqlite::Connection, table: &str) -> Result<Vec<String>, rusqlite::Error> {
    let mut statement = conn.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")?;
    let names = statement
        .query_map([table], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(names)
}
