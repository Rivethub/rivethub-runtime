use jiff::Timestamp;
use kernel::memory::EmbedTable;
use rusqlite::Connection;
use rusqlite::TransactionBehavior;
use rusqlite::params;
use serde::Serialize;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::meta;
use crate::meta::MetaKey;
use crate::queue::EMBED_TASK;
use crate::queue::JobState;
use crate::queue::enqueue_embed;

const TARGETS: [(EmbedTable, &str); 3] = [
    (EmbedTable::Messages, "id"),
    (EmbedTable::Summaries, "id"),
    (EmbedTable::WikiTopics, "slug"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReembedReport {
    pub cleared: u64,
    pub queued: u64,
}

impl SqliteStore {
    pub async fn reembed(&self, apply: bool) -> Result<ReembedReport, SqliteError> {
        let now = self.now();
        let report = self
            .with_conn(move |conn| {
                let tx = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(SqliteError::query("reembed"))?;
                let report = reset(&tx, now)?;
                if apply {
                    tx.commit().map_err(SqliteError::query("reembed"))?;
                } else {
                    tx.rollback().map_err(SqliteError::query("reembed"))?;
                }
                Ok(report)
            })
            .await?;
        if apply && report.queued > 0 {
            self.notifications.send_replace(());
        }
        Ok(report)
    }
}

fn reset(conn: &Connection, now: Timestamp) -> Result<ReembedReport, SqliteError> {
    conn.execute(
        "DELETE FROM ros_jobs WHERE task = ?1 AND state <> ?2",
        params![EMBED_TASK, JobState::Running.to_string()],
    )
    .map_err(SqliteError::query("reembed jobs"))?;
    let mut report = ReembedReport {
        cleared: 0,
        queued: 0,
    };
    for (table, key) in TARGETS {
        report.cleared += clear(conn, table)?;
        for id in eligible(conn, table, key)? {
            if enqueue_embed(conn, &table.to_string(), &id, now)? {
                report.queued += 1;
            }
        }
    }
    meta::forget(conn, MetaKey::EmbedModel)?;
    meta::forget(conn, MetaKey::EmbedDims)?;
    Ok(report)
}

fn clear(conn: &Connection, table: EmbedTable) -> Result<u64, SqliteError> {
    let held: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM {table} WHERE embedding IS NOT NULL"),
            [],
            |row| row.get(0),
        )
        .map_err(SqliteError::query("reembed count"))?;
    conn.execute(
        &format!(
            "UPDATE {table} SET embedding = NULL, embed_status = NULL, embed_error = NULL, \
             embed_failures = 0 WHERE embed_status IS NOT 'unembeddable'"
        ),
        [],
    )
    .map_err(SqliteError::query("reembed clear"))?;
    u64::try_from(held).map_err(SqliteError::Count)
}

fn eligible(conn: &Connection, table: EmbedTable, key: &str) -> Result<Vec<String>, SqliteError> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {key} FROM {table} WHERE embed_status IS NOT 'unembeddable' ORDER BY rowid"
        ))
        .map_err(SqliteError::query("reembed targets"))?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(SqliteError::query("reembed targets"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("reembed targets"))?;
    Ok(ids)
}
