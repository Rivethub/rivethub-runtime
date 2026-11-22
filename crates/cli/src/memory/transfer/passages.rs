use std::collections::BTreeSet;

use rusqlite::Connection;
use sqlx::PgConnection;

use super::catalog::Relation;
use super::catalog::Table;
use super::values::Kind;
use super::{Batch, Row, cell, pg, position};
use crate::Error;

pub(super) fn align(batches: &mut [Batch], key: Option<[u8; 32]>) {
    let key = key.map(hex::encode);
    let indexed = compatible_summaries(batches, key.as_deref());
    let Some(batch) = batches
        .iter_mut()
        .find(|batch| batch.table.relation == Relation::SummaryPassages)
    else {
        return;
    };
    let index_key = position(&batch.table, "index_key");
    let record = position(&batch.table, "record_id");
    let embedding = position(&batch.table, "embedding");
    for row in &mut batch.rows {
        let compatible = key.as_deref() == cell(row, index_key)
            && cell(row, record).is_some_and(|id| indexed.contains(id));
        if !compatible && let Some(value) = embedding.and_then(|index| row.get_mut(index)) {
            *value = None;
        }
    }
}

fn compatible_summaries(batches: &[Batch], key: Option<&str>) -> BTreeSet<String> {
    let (Some(batch), Some(key)) = (
        batches
            .iter()
            .find(|batch| batch.table.relation == Relation::Summaries),
        key,
    ) else {
        return BTreeSet::new();
    };
    let id = position(&batch.table, "id");
    let stored_key = position(&batch.table, "index_key");
    batch
        .rows
        .iter()
        .filter(|row| cell(row, stored_key) == Some(key))
        .filter_map(|row| cell(row, id).map(str::to_owned))
        .collect()
}

fn record_ids(batch: &Batch) -> BTreeSet<&str> {
    let record = position(&batch.table, "record_id");
    batch
        .rows
        .iter()
        .filter_map(|row| cell(row, record))
        .collect()
}

pub(super) async fn finish_postgres(
    conn: &mut PgConnection,
    batch: &Batch,
    key: &[u8; 32],
) -> Result<(), Error> {
    sqlx::query(
        "UPDATE rh_summary_passages p SET embedding = s.embedding
         FROM transfer_rh_summary_passages s
         WHERE p.id = s.id AND p.embedding IS NULL AND s.embedding IS NOT NULL",
    )
    .execute(&mut *conn)
    .await
    .map_err(pg::error)?;
    let records = record_ids(batch)
        .into_iter()
        .map(str::parse::<uuid::Uuid>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| super::invalid(format!("summary passage record id: {error}")))?;
    sqlx::query!(
        "UPDATE rh_summaries s SET index_needs_repair = false
             WHERE s.id = ANY($2) AND s.kind IN ('session', 'part') AND s.index_key = $1
               AND s.index_needs_repair AND rh_summary_record_indexed(s.id)",
        key.as_slice(),
        &records
    )
    .execute(conn)
    .await
    .map_err(pg::error)?;
    Ok(())
}

pub(super) fn fill_sqlite(
    conn: &Connection,
    table: &Table,
    row: &Row,
) -> Result<(), memory_sqlite::SqliteError> {
    let Some(vector) = cell(row, position(table, "embedding")) else {
        return Ok(());
    };
    let vector = Kind::Vector
        .bind(Some(vector))
        .map_err(memory_sqlite::SqliteError::Pragma)?;
    let id = cell(row, position(table, "id"));
    conn.prepare_cached(
        "UPDATE ros_summary_passages SET embedding = ?2 WHERE id = ?1 AND embedding IS NULL",
    )
    .and_then(|mut statement| statement.execute(rusqlite::params![id, vector]))
    .map_err(memory_sqlite::SqliteError::Pragma)?;
    Ok(())
}

pub(super) fn finish_sqlite(
    conn: &Connection,
    batch: &Batch,
    key: &[u8; 32],
) -> Result<(), memory_sqlite::SqliteError> {
    let mut update = conn
        .prepare(
            "UPDATE ros_summaries SET index_needs_repair = 0
             WHERE id = ?1 AND index_key = ?2 AND index_needs_repair
               AND id IN (SELECT id FROM ros_summary_records_indexed WHERE id = ?1)",
        )
        .map_err(memory_sqlite::SqliteError::Pragma)?;
    for id in record_ids(batch) {
        update
            .execute(rusqlite::params![id, key.as_slice()])
            .map_err(memory_sqlite::SqliteError::Pragma)?;
    }
    Ok(())
}
