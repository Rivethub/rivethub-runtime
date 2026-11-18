use kernel::memory::{
    SummaryKind, SummaryStoreError,
    summary_backend::{SummaryInput, SummaryRun},
    summary_reference::{PreparedSummaryRecord, SummaryManifestEntry},
};
use serde_json::Value;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::{PostgresMemory, settlement::INCLUDED};

pub(crate) fn error(source: impl std::error::Error + Send + Sync + 'static) -> SummaryStoreError {
    SummaryStoreError::Query {
        message: "live summary workspace".into(),
        source: Box::new(source),
    }
}

pub(crate) const ELIGIBLE: &str = "s.input_revision = $2 AND s.settled_at IS NOT NULL
    AND s.summarized_through IS DISTINCT FROM $2 AND s.failed_revision IS DISTINCT FROM $2";

impl PostgresMemory {
    pub(crate) async fn live_source(
        &self,
        id: Uuid,
    ) -> Result<Option<SummaryInput>, SummaryStoreError> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(error)?;
        let owner = sqlx::query_scalar("SELECT owner_user_id FROM rh_conversations WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(error)?;
        let Some(owner) = owner else { return Ok(None) };
        let rows: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT id, kind FROM rh_summaries WHERE conversation_id = $1")
                .bind(id)
                .fetch_all(&mut *tx)
                .await
                .map_err(error)?;
        let expected_root = rows
            .iter()
            .find(|(_, kind)| matches!(kind.as_str(), "session" | "root"))
            .map(|(id, _)| *id);
        let expected_ids = rows.into_iter().map(|(id, _)| id).collect();
        let messages = crate::session_messages::summary_messages(&mut tx, id)
            .await
            .map_err(error)?;
        let parts = parts(&mut tx, id).await.map_err(error)?;
        tx.commit().await.map_err(error)?;
        Ok(Some(SummaryInput {
            owner,
            expected_ids,
            expected_root,
            messages,
            parts,
        }))
    }
}

async fn parts(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Vec<PreparedSummaryRecord>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, parent_id, summary_ordinal, content, input_key, index_key, model,
                issued_part_ordinal, input_token_estimate, quality_flags
         FROM rh_summaries WHERE conversation_id = $1 AND kind = 'part' ORDER BY summary_ordinal",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let mut parts = Vec::new();
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let links: Vec<(Uuid, i32, Option<Vec<u8>>)> = sqlx::query_as(
            "SELECT message_id, ordinal, render_key FROM rh_summary_sources WHERE summary_id = $1 ORDER BY ordinal"
        ).bind(id).fetch_all(&mut *conn).await?;
        let Some(input_key) = key(row.try_get("input_key")?) else {
            continue;
        };
        parts.push(PreparedSummaryRecord {
            id,
            parent: row.try_get("parent_id")?,
            kind: SummaryKind::Part,
            ordinal: row.try_get("summary_ordinal")?,
            content: row.try_get("content")?,
            input_key,
            index_key: key(row.try_get("index_key")?),
            model: row.try_get("model")?,
            issued_part_ordinal: row.try_get("issued_part_ordinal")?,
            input_tokens: row.try_get("input_token_estimate")?,
            quality_flags: row.try_get("quality_flags")?,
            section: None,
            sources: links
                .into_iter()
                .map(|(message, ordinal, render)| SummaryManifestEntry {
                    message,
                    ordinal,
                    render_key: key(render),
                })
                .collect(),
        });
    }
    Ok(parts)
}

fn key(bytes: Option<Vec<u8>>) -> Option<[u8; 32]> {
    bytes.and_then(|bytes| bytes.try_into().ok())
}

pub(crate) async fn eligible(
    conn: &mut PgConnection,
    id: Uuid,
    revision: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT EXISTS (SELECT 1 FROM rh_summary_state s JOIN rh_conversations c ON c.id = s.conversation_id
         WHERE c.id = $1 AND {ELIGIBLE} AND {INCLUDED})"
    ))).bind(id).bind(revision).fetch_one(conn).await
}

pub(crate) async fn candidates(
    store: &PostgresMemory,
    after: Uuid,
    limit: i64,
) -> Result<Vec<(Uuid, i64)>, SummaryStoreError> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT c.id, s.input_revision FROM rh_summary_state s JOIN rh_conversations c ON c.id = s.conversation_id
         WHERE c.id > $1 AND s.settled_at IS NOT NULL AND NOT s.historical_only
           AND s.input_revision IS DISTINCT FROM s.summarized_through AND s.input_revision IS DISTINCT FROM s.failed_revision
           AND NOT EXISTS (
             SELECT 1 FROM graphile_worker._private_jobs j
             WHERE j.key = 'compact-' || c.id::text AND (j.locked_at IS NOT NULL OR j.attempts < j.max_attempts))
           AND {INCLUDED} ORDER BY c.id LIMIT $2"
    ))).bind(after).bind(limit).fetch_all(&store.pool).await.map_err(error)
}

pub(crate) async fn run(
    store: &PostgresMemory,
    id: Uuid,
) -> Result<Option<SummaryRun>, SummaryStoreError> {
    let row: Option<(Uuid, i64, Value)> = sqlx::query_as(
        "SELECT id, revision, manifest FROM rh_summary_runs WHERE conversation_id = $1",
    )
    .bind(id)
    .fetch_optional(&store.pool)
    .await
    .map_err(error)?;
    Ok(row.map(|(id, revision, manifest)| SummaryRun {
        id,
        revision,
        manifest,
    }))
}
