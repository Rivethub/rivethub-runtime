use std::collections::BTreeSet;

use kernel::memory::summary_reference::{SummaryConsumer, SummaryReferenceError};
use sqlx::{Connection as _, PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

const LOCK_CLASS: i32 = 0x7375_6d6d;
const APPLY_LOCK_CLASS: i32 = 0x7361_7070;

#[derive(Debug, thiserror::Error)]
pub(crate) enum FenceError {
    #[error(transparent)]
    Reference(#[from] SummaryReferenceError),
    #[error(transparent)]
    Query(#[from] sqlx::Error),
}

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct SummaryInput {
    pub(crate) id: Uuid,
    pub(crate) conversation_id: Option<Uuid>,
    pub(crate) owner_user_id: String,
    pub(crate) content: String,
    pub(crate) input_key: Option<Vec<u8>>,
}

async fn inputs(conn: &mut PgConnection, ids: &[Uuid]) -> Result<Vec<SummaryInput>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, conversation_id, owner_user_id, content, input_key
         FROM rh_summaries WHERE id = ANY($1) ORDER BY id",
    )
    .bind(ids)
    .fetch_all(conn)
    .await
}

pub(crate) async fn lock_conversation(
    conn: &mut PgConnection,
    conversation: Uuid,
    exclusive: bool,
) -> Result<(), sqlx::Error> {
    let sql = if exclusive {
        "SELECT pg_advisory_xact_lock($1, hashtext($2::text))"
    } else {
        "SELECT pg_advisory_xact_lock_shared($1, hashtext($2::text))"
    };
    sqlx::query(sql)
        .bind(LOCK_CLASS)
        .bind(conversation.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

fn find_input<'a>(rows: &'a [SummaryInput], id: &Uuid) -> Option<&'a SummaryInput> {
    let index = rows.partition_point(|input| input.id < *id);
    rows.get(index).filter(|input| input.id == *id)
}

pub(crate) async fn shared_visible(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<Vec<SummaryInput>, FenceError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let before = inputs(tx, ids).await?;
    let keys: BTreeSet<_> = before
        .iter()
        .map(|input| input.conversation_id.unwrap_or(input.id))
        .collect();
    for key in keys {
        lock_conversation(tx, key, false).await?;
    }
    let after = inputs(tx, ids).await?;
    let mut guarded = Vec::with_capacity(after.len());
    for current in after {
        let Some(old) = find_input(&before, &current.id) else {
            continue;
        };
        if old != &current {
            return Err(SummaryReferenceError::Stale(old.id).into());
        }
        guarded.push(current);
    }
    Ok(guarded)
}

pub(crate) async fn shared(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<Vec<SummaryInput>, FenceError> {
    let guarded = shared_visible(tx, ids).await?;
    for id in ids {
        if guarded.binary_search_by_key(id, |input| input.id).is_err() {
            return Err(SummaryReferenceError::Gone(*id).into());
        }
    }
    Ok(guarded)
}

pub(crate) async fn pending(
    conn: &mut PgConnection,
    anchor: Uuid,
    consumer: SummaryConsumer,
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT d.message_id FROM rh_summary_deliveries d
         JOIN rh_summary_sources s ON s.summary_id = d.anchor_id AND s.message_id = d.message_id
         WHERE d.anchor_id = $1 AND d.consumer = $2 ORDER BY s.ordinal, d.message_id",
    )
    .bind(anchor)
    .bind(consumer.as_str())
    .fetch_all(conn)
    .await
}

pub(crate) async fn complete(
    tx: &mut Transaction<'_, Postgres>,
    anchor: Uuid,
    consumer: SummaryConsumer,
    expected: &[Uuid],
    version: i32,
) -> Result<(), FenceError> {
    if pending(tx, anchor, consumer).await? != expected {
        return Err(SummaryReferenceError::Stale(anchor).into());
    }
    sqlx::query(
        "UPDATE rh_summary_deliveries SET anchor_id = NULL, completed_version = $4
         WHERE anchor_id = $1 AND consumer = $2 AND message_id = ANY($3)",
    )
    .bind(anchor)
    .bind(consumer.as_str())
    .bind(expected)
    .bind(version)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) async fn complete_legacy(
    tx: &mut Transaction<'_, Postgres>,
    anchor: Uuid,
    consumer: SummaryConsumer,
    version: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_summary_deliveries (conversation_id, consumer, message_id, completed_version)
         SELECT s.conversation_id, $2, l.message_id, $3 FROM rh_summaries s
         JOIN rh_summary_sources l ON l.summary_id = s.id
         JOIN rh_messages m ON m.id = l.message_id AND m.conversation_id = s.conversation_id
         WHERE s.id = $1 AND s.kind = 'leaf' AND rh_scope_maintenance()
         ON CONFLICT (conversation_id, consumer, message_id) DO NOTHING",
    )
    .bind(anchor)
    .bind(consumer.as_str())
    .bind(version)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) mod publish;
mod retain;
pub use publish::ReplaceSummaryError;

pub(crate) async fn exclusive(
    conn: &mut PgConnection,
    conversations: impl IntoIterator<Item = Uuid>,
) -> Result<(), sqlx::Error> {
    let ordered: BTreeSet<_> = conversations.into_iter().collect();
    for id in &ordered {
        sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2::text))")
            .bind(APPLY_LOCK_CLASS)
            .bind(id.to_string())
            .execute(&mut *conn)
            .await?;
    }
    for id in ordered {
        lock_conversation(conn, id, true).await?;
    }
    Ok(())
}

pub(crate) async fn external(
    pool: &PgPool,
    ids: &[Uuid],
) -> Result<(PgConnection, Vec<SummaryInput>), FenceError> {
    let mut conn = PgConnection::connect_with(pool.connect_options().as_ref()).await?;
    crate::scope::apply(&mut conn).await?;
    let before = inputs(&mut conn, ids).await?;
    let keys: BTreeSet<_> = before
        .iter()
        .map(|input| input.conversation_id.unwrap_or(input.id))
        .collect();
    for key in keys {
        sqlx::query("SELECT pg_advisory_lock_shared($1, hashtext($2::text))")
            .bind(APPLY_LOCK_CLASS)
            .bind(key.to_string())
            .execute(&mut conn)
            .await?;
    }
    let after = inputs(&mut conn, ids).await?;
    for id in ids {
        let (Some(old), Some(current)) = (find_input(&before, id), find_input(&after, id)) else {
            return Err(SummaryReferenceError::Gone(*id).into());
        };
        if old != current {
            return Err(SummaryReferenceError::Stale(*id).into());
        }
    }
    Ok((conn, after))
}

mod sql;
