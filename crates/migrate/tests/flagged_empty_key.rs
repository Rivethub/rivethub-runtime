type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn the_ledger_step_takes_a_dead_job_with_an_empty_key() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let old = rusqlite::Connection::open(&path)?;
    old.execute_batch(
        "DROP TABLE rh_flagged_jobs;
         DROP TABLE rh_job_failures;
         INSERT INTO ros_jobs
            (id, task, job_key, payload, run_at, attempts, max_attempts, state, created_at, updated_at)
         VALUES
            ('dead-empty', 'embed-target', '', '{}', '2026-01-01T00:00:00.000Z', 2, 2, 'dead',
             '2026-01-01T00:00:00.000Z', '2026-01-02T00:00:00.000Z');
         PRAGMA user_version = 45;",
    )?;
    drop(old);
    let upgraded = migrate::sqlite(&path)?;
    assert_eq!(upgraded.current, memory_sqlite::SCHEMA_VERSION);
    let conn = rusqlite::Connection::open(&path)?;
    let moved: (i64, i64, i64) = conn.query_row(
        "SELECT (SELECT count(*) FROM rh_flagged_jobs WHERE job_key IS NULL),
                (SELECT count(*) FROM rh_job_failures),
                (SELECT count(*) FROM ros_jobs WHERE state = 'dead')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(moved, (1, 0, 0));
    Ok(())
}
