use kernel::memory::summary_reference::{SummaryConsumer, SummaryReferenceError};
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub(crate) enum FenceError {
    #[error(transparent)]
    Reference(#[from] SummaryReferenceError),
    #[error(transparent)]
    Query(#[from] rusqlite::Error),
}

#[derive(Debug)]
pub(crate) struct SummaryInput {
    pub(crate) conversation_id: Option<String>,
    pub(crate) owner: Option<String>,
    pub(crate) content: String,
    pub(crate) input_key: Option<Vec<u8>>,
}

pub(crate) fn immediate(conn: &mut Connection) -> Result<Transaction<'_>, rusqlite::Error> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
}

pub(crate) fn shared(tx: &Transaction<'_>, id: Uuid) -> Result<SummaryInput, FenceError> {
    tx.query_row(
        "SELECT s.conversation_id, c.owner_user_id, s.content, s.input_key FROM ros_summaries s
         LEFT JOIN ros_conversations c ON c.id = s.conversation_id WHERE s.id = ?1",
        [id.to_string()],
        |row| {
            Ok(SummaryInput {
                conversation_id: row.get(0)?,
                owner: row.get(1)?,
                content: row.get(2)?,
                input_key: row.get(3)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| SummaryReferenceError::Gone(id).into())
}

pub(crate) fn pending(
    conn: &Connection,
    anchor: Uuid,
    consumer: SummaryConsumer,
) -> Result<Vec<Uuid>, rusqlite::Error> {
    conn.prepare(
        "SELECT d.message_id FROM ros_summary_deliveries d
         JOIN ros_summary_sources s ON s.summary_id = d.anchor_id AND s.message_id = d.message_id
         WHERE d.anchor_id = ?1 AND d.consumer = ?2 ORDER BY s.ordinal, d.message_id",
    )?
    .query_map(params![anchor.to_string(), consumer.as_str()], |row| {
        let id: String = row.get(0)?;
        Uuid::parse_str(&id).map_err(|source| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(source),
            )
        })
    })?
    .collect()
}

pub(crate) fn complete(
    tx: &Transaction<'_>,
    anchor: Uuid,
    consumer: SummaryConsumer,
    expected: &[Uuid],
    version: i32,
) -> Result<(), FenceError> {
    if pending(tx, anchor, consumer)? != expected {
        return Err(SummaryReferenceError::Stale(anchor).into());
    }
    tx.execute(
        "UPDATE ros_summary_deliveries SET anchor_id = NULL, completed_version = ?3
         WHERE anchor_id = ?1 AND consumer = ?2",
        params![anchor.to_string(), consumer.as_str(), version],
    )?;
    Ok(())
}

pub(crate) fn complete_legacy(
    tx: &Transaction<'_>,
    anchor: Uuid,
    consumer: SummaryConsumer,
    version: i32,
) -> Result<(), rusqlite::Error> {
    tx.execute(
        "INSERT INTO ros_summary_deliveries (conversation_id, consumer, message_id, completed_version)
         SELECT s.conversation_id, ?2, l.message_id, ?3 FROM ros_summaries s
         JOIN ros_summary_sources l ON l.summary_id = s.id
         JOIN ros_messages m ON m.id = l.message_id AND m.conversation_id = s.conversation_id
         WHERE s.id = ?1 AND s.kind = 'leaf'
         ON CONFLICT (conversation_id, consumer, message_id) DO NOTHING",
        params![anchor.to_string(), consumer.as_str(), version],
    )?;
    Ok(())
}
mod passages;
pub(crate) mod publish;
mod retain;

pub(crate) fn live_job(
    tx: &Transaction<'_>,
    task: &str,
    payload: &str,
) -> Result<bool, crate::SqliteError> {
    let payload: &serde_json::value::RawValue = match serde_json::from_str(payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::debug!(error_kind = ?error.classify(), "skipping summary fence for unreadable job payload");
            return Ok(true);
        }
    };
    let Some(id) = kernel::memory::summary_reference::summary_job_target(task, payload) else {
        return Ok(true);
    };
    match shared(tx, id) {
        Ok(_) => Ok(true),
        Err(FenceError::Reference(_)) => Ok(false),
        Err(FenceError::Query(error)) => {
            Err(crate::SqliteError::query("summary job target")(error))
        }
    }
}

mod sql;
