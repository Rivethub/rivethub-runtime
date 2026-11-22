use std::str::FromStr;

use async_trait::async_trait;
use kernel::queue::JobQueueError;
use kernel::queue::pause::PausableRole;
use kernel::queue::pause::RolePause;
use kernel::queue::pause::WorkerPauses;

use crate::PostgresMemory;
use crate::error::Error;

type PauseRow = (String, jiff_sqlx::Timestamp, Option<String>);

fn failed(source: sqlx::Error) -> JobQueueError {
    JobQueueError::from(Error::operation("worker pauses", source))
}

fn pause(role: PausableRole, (_, paused_at, paused_by): PauseRow) -> RolePause {
    RolePause {
        role,
        paused_at: paused_at.to_jiff(),
        paused_by,
    }
}

#[async_trait]
impl WorkerPauses for PostgresMemory {
    async fn paused(&self) -> Result<Vec<RolePause>, JobQueueError> {
        let rows: Vec<PauseRow> =
            sqlx::query_as("SELECT role, paused_at, paused_by FROM rh_worker_pauses ORDER BY role")
                .fetch_all(&self.pool)
                .await
                .map_err(failed)?;
        rows.into_iter()
            .map(|row| {
                let role = PausableRole::from_str(&row.0)
                    .map_err(|unpausable| JobQueueError::Stored(unpausable.to_string()))?;
                Ok(pause(role, row))
            })
            .collect()
    }

    async fn pause(&self, role: PausableRole, by: &str) -> Result<RolePause, JobQueueError> {
        let row: PauseRow = sqlx::query_as(
            "WITH added AS (
                 INSERT INTO rh_worker_pauses (role, paused_at, paused_by) VALUES ($1, now(), $2)
                 ON CONFLICT (role) DO NOTHING
                 RETURNING role, paused_at, paused_by)
             SELECT role, paused_at, paused_by FROM added
             UNION ALL
             SELECT role, paused_at, paused_by FROM rh_worker_pauses WHERE role = $1
             LIMIT 1",
        )
        .bind(role.to_string())
        .bind(by)
        .fetch_one(&self.pool)
        .await
        .map_err(failed)?;
        Ok(pause(role, row))
    }

    async fn resume(&self, role: PausableRole) -> Result<(), JobQueueError> {
        sqlx::query(
            "WITH resumed AS (DELETE FROM rh_worker_pauses WHERE role = $1 RETURNING role)
             SELECT pg_notify('jobs:insert', '{\"r\":' || random()::text || ',\"count\":1}')
               FROM resumed",
        )
        .bind(role.to_string())
        .execute(&self.pool)
        .await
        .map_err(failed)?;
        Ok(())
    }
}
