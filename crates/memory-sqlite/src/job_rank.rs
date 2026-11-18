use jiff::Timestamp;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params;

use crate::error::SqliteError;
use crate::time;

pub(crate) struct DueRow {
    pub(crate) id: String,
    pub(crate) task: String,
    pub(crate) key: Option<String>,
    pub(crate) payload: String,
    pub(crate) max_attempts: i64,
}

pub(crate) fn rank(conn: &Connection, id: &str, priority: i16) -> Result<(), SqliteError> {
    let written = if priority == 0 {
        conn.execute("DELETE FROM rh_job_priorities WHERE job_id = ?1", [id])
    } else {
        conn.execute(
            "INSERT INTO rh_job_priorities (job_id, priority) VALUES (?1, ?2)
             ON CONFLICT (job_id) DO UPDATE SET priority = excluded.priority",
            params![id, priority],
        )
    };
    written
        .map(|_| ())
        .map_err(SqliteError::query("job priority"))
}

pub(crate) fn rank_key(conn: &Connection, key: &str, priority: i16) -> Result<(), SqliteError> {
    let id: Option<String> = conn
        .query_row("SELECT id FROM ros_jobs WHERE job_key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()
        .map_err(SqliteError::query("job priority"))?;
    match id {
        Some(id) => rank(conn, &id, priority),
        None => Err(SqliteError::MissingJob { id: key.to_owned() }),
    }
}

pub(crate) fn select_due(
    conn: &Connection,
    tasks: &[String],
    only_id: Option<&str>,
    now: Timestamp,
) -> Result<Option<DueRow>, SqliteError> {
    let ranked = due(
        conn,
        &due_sql(tasks.len(), Due::Ranked),
        tasks,
        only_id,
        now,
    )?;
    match ranked {
        Some((row, priority)) if priority < 0 => Ok(Some(row)),
        ranked => match due(
            conn,
            &due_sql(tasks.len(), Due::Default),
            tasks,
            only_id,
            now,
        )? {
            Some((row, _)) => Ok(Some(row)),
            None => Ok(ranked.map(|(row, _)| row)),
        },
    }
}

#[derive(Clone, Copy)]
enum Due {
    Ranked,
    Default,
}

fn due(
    conn: &Connection,
    sql: &str,
    tasks: &[String],
    only_id: Option<&str>,
    now: Timestamp,
) -> Result<Option<(DueRow, i64)>, SqliteError> {
    let stamped = time::stamp(now);
    let pinned = only_id.map(str::to_owned);
    let mut bound: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(tasks.len().saturating_add(2));
    bound.push(&stamped);
    for task in tasks {
        bound.push(task);
    }
    bound.push(&pinned);
    conn.query_row(sql, bound.as_slice(), read_due)
        .optional()
        .map_err(SqliteError::query("job claim"))
}

fn due_sql(count: usize, due: Due) -> String {
    let slots = (0..count)
        .map(|index| format!("?{}", index.saturating_add(2)))
        .collect::<Vec<_>>()
        .join(", ");
    let pin = count.saturating_add(2);
    let (rank, from, unranked, order) = match due {
        Due::Ranked => (
            "p.priority",
            "rh_job_priorities p JOIN ros_jobs j ON j.id = p.job_id",
            "",
            "p.priority, ",
        ),
        Due::Default => (
            "0",
            "ros_jobs j",
            " AND NOT EXISTS (SELECT 1 FROM rh_job_priorities p WHERE p.job_id = j.id)",
            "",
        ),
    };
    format!(
        "SELECT j.id, j.task, j.job_key, j.payload, j.max_attempts, {rank} FROM {from} \
         WHERE j.state = 'queued' AND j.run_at <= ?1 AND j.task IN ({slots}) \
         AND (?{pin} IS NULL OR j.id = ?{pin}){unranked} \
         ORDER BY {order}j.run_at, j.rowid LIMIT 1"
    )
}

fn read_due(row: &rusqlite::Row<'_>) -> rusqlite::Result<(DueRow, i64)> {
    Ok((
        DueRow {
            id: row.get(0)?,
            task: row.get(1)?,
            key: row.get(2)?,
            payload: row.get(3)?,
            max_attempts: row.get(4)?,
        },
        row.get(5)?,
    ))
}
