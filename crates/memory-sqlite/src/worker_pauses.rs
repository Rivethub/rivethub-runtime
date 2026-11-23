use std::str::FromStr;

use async_trait::async_trait;
use jiff::Timestamp;
use kernel::queue::JobQueueError;
use kernel::queue::pause::PausableRole;
use kernel::queue::pause::RolePause;
use kernel::queue::pause::WorkerPauses;
use rusqlite::Connection;
use rusqlite::params;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

#[async_trait]
impl WorkerPauses for SqliteStore {
    async fn paused(&self) -> Result<Vec<RolePause>, JobQueueError> {
        self.with_conn(paused_rows)
            .await
            .map_err(pause_error)?
            .into_iter()
            .map(pause_from)
            .collect()
    }

    async fn pause(&self, role: PausableRole, by: &str) -> Result<RolePause, JobQueueError> {
        let at = self.clock.now();
        let by = by.to_owned();
        let row = self
            .with_conn(move |conn| insert_pause(conn, role, &by, at))
            .await
            .map_err(pause_error)?;
        crate::job_notify::record_baseline(self).await?;
        self.notifications.send_replace(());
        pause_from(row)
    }

    async fn resume(&self, role: PausableRole) -> Result<(), JobQueueError> {
        let deleted = self
            .with_conn(move |conn| delete_pause(conn, &role.to_string()))
            .await
            .map_err(pause_error)?;
        if deleted > 0 {
            crate::job_notify::record_baseline(self).await?;
            self.notifications.send_replace(());
        }
        Ok(())
    }
}

struct PauseRow {
    stored: String,
    paused_at: String,
    paused_by: Option<String>,
}

fn paused_rows(conn: &mut Connection) -> Result<Vec<PauseRow>, SqliteError> {
    let mut statement = conn
        .prepare("SELECT role, paused_at, paused_by FROM rh_worker_pauses ORDER BY role")
        .map_err(SqliteError::query("worker pauses"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(PauseRow {
                stored: row.get(0)?,
                paused_at: row.get(1)?,
                paused_by: row.get(2)?,
            })
        })
        .map_err(SqliteError::query("worker pauses"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("worker pauses"))
}

fn insert_pause(
    conn: &Connection,
    role: PausableRole,
    by: &str,
    at: Timestamp,
) -> Result<PauseRow, SqliteError> {
    conn.query_row(
        "INSERT INTO rh_worker_pauses (role, paused_at, paused_by) VALUES (?1, ?2, ?3)
         ON CONFLICT (role) DO UPDATE SET role = excluded.role
         RETURNING role, paused_at, paused_by",
        params![role.to_string(), time::stamp(at), by],
        |row| {
            Ok(PauseRow {
                stored: row.get(0)?,
                paused_at: row.get(1)?,
                paused_by: row.get(2)?,
            })
        },
    )
    .map_err(SqliteError::query("worker pauses"))
}

fn delete_pause(conn: &Connection, role: &str) -> Result<usize, SqliteError> {
    conn.execute("DELETE FROM rh_worker_pauses WHERE role = ?1", [role])
        .map_err(SqliteError::query("worker pauses"))
}

fn pause_from(row: PauseRow) -> Result<RolePause, JobQueueError> {
    let role = PausableRole::from_str(&row.stored)
        .map_err(|unpausable| JobQueueError::Stored(unpausable.to_string()))?;
    let paused_at = time::stored(&row.paused_at, "paused_at").map_err(pause_error)?;
    Ok(RolePause {
        role,
        paused_at,
        paused_by: row.paused_by,
    })
}

fn pause_error(error: SqliteError) -> JobQueueError {
    if crate::queue_admin::busy(&error) {
        return JobQueueError::Busy(Box::new(error));
    }
    JobQueueError::Operation {
        operation: "worker pauses",
        source: Box::new(error),
    }
}
