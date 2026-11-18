use chrono::{DateTime, Utc};
use kernel::filing::Target;
use kernel::topics::TopicId;
use kernel::wiki::builder::{DeferredWiki, ExtractWiki, WikiStoreError};
use kernel::wiki::page::WikiPatch;
use kernel::wiki::page::WikiSlug;
use sqlx::PgPool;
use uuid::Uuid;
use wire::session_id::SessionId;

use super::{load_summary, query_error, topics};

#[derive(Debug, sqlx::FromRow)]
struct DeferredRow {
    summary_id: Uuid,
    waits_session: Option<String>,
    waits_conversation: Option<Uuid>,
    applied_topic: Option<Uuid>,
    patches: String,
    deferred_at: DateTime<Utc>,
}

pub(super) async fn defer(pool: &PgPool, deferred: DeferredWiki) -> Result<(), WikiStoreError> {
    let mut tx = pool.begin().await.map_err(query_error)?;
    guard(&mut tx, deferred.summary_id).await?;
    upsert(&mut tx, &deferred).await?;
    crate::summary_replace::complete_legacy(
        &mut tx,
        deferred.summary_id,
        kernel::memory::summary_reference::SummaryConsumer::Wiki,
        kernel::wiki::builder::WIKI_PIPELINE_VERSION,
    )
    .await
    .map_err(query_error)?;
    tx.commit().await.map_err(query_error)
}

pub(super) async fn unsettled(
    pool: &PgPool,
    limit: u32,
) -> Result<Vec<DeferredWiki>, WikiStoreError> {
    sqlx::query_as::<_, DeferredRow>(
        "SELECT d.summary_id, d.waits_session, d.waits_conversation, d.applied_topic,
                d.patches::text AS patches, d.deferred_at
           FROM rh_wiki_deferred d
           LEFT JOIN rh_session_placements p ON p.session_id = d.waits_session
           LEFT JOIN rh_conversation_placements c ON c.conversation_id = d.waits_conversation
          WHERE COALESCE(p.topic_id, c.topic_id) IS DISTINCT FROM d.applied_topic
          ORDER BY d.deferred_at, d.summary_id LIMIT $1",
    )
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
    .map_err(query_error)?
    .into_iter()
    .map(|row| {
        Ok(DeferredWiki {
            summary_id: row.summary_id,
            waits: Target::stored(row.waits_session.as_deref(), row.waits_conversation)
                .ok_or_else(|| {
                    invalid("a deferral waits on neither a session nor a conversation")
                })?,
            applied: row.applied_topic.map(TopicId::from_uuid),
            patches: serde_json::from_str::<Vec<WikiPatch>>(&row.patches).map_err(invalid)?,
            at: row.deferred_at,
        })
    })
    .collect()
}

pub(super) async fn cite(
    pool: &PgPool,
    summary_id: Uuid,
    slug: &WikiSlug,
    now: DateTime<Utc>,
) -> Result<(), WikiStoreError> {
    let mut tx = pool.begin().await.map_err(query_error)?;
    let job = ExtractWiki {
        summary_id,
        conversation_id: None,
    };
    guard(&mut tx, summary_id).await?;
    let summary = load_summary(&mut tx, &job, false).await?;
    topics::cite(&mut tx, slug.as_ref(), &summary, now).await?;
    tx.commit().await.map_err(query_error)
}

pub(super) async fn uncite(
    pool: &PgPool,
    summary_id: Uuid,
    slug: &WikiSlug,
) -> Result<(), WikiStoreError> {
    let mut tx = pool.begin().await.map_err(query_error)?;
    guard(&mut tx, summary_id).await?;
    for statement in [
        "DELETE FROM rh_wiki_citations WHERE topic_slug = $1 AND summary_id = $2
            AND NOT EXISTS (SELECT 1 FROM rh_wiki_extractions e
                             WHERE e.summary_id = $2 AND $1 = ANY (e.topics_touched))",
        "DELETE FROM rh_wiki_provenance
          WHERE topic_slug = $1 AND source_kind = 'summary' AND source_id = $2
            AND NOT EXISTS (SELECT 1 FROM rh_wiki_extractions e
                             WHERE e.summary_id = $2 AND $1 = ANY (e.topics_touched))",
    ] {
        sqlx::query(statement)
            .bind(slug.as_ref())
            .bind(summary_id)
            .execute(&mut *tx)
            .await
            .map_err(query_error)?;
    }
    tx.commit().await.map_err(query_error)
}

fn encoded<T: serde::Serialize>(value: &T) -> Result<String, WikiStoreError> {
    serde_json::to_string(value).map_err(invalid)
}

fn invalid(error: impl std::fmt::Display) -> WikiStoreError {
    WikiStoreError::InvalidPatch(error.to_string())
}

async fn guard(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<(), WikiStoreError> {
    crate::summary_replace::shared(tx, &[id])
        .await
        .map_err(super::fence_error)?;
    Ok(())
}

pub(super) async fn upsert(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    deferred: &DeferredWiki,
) -> Result<(), WikiStoreError> {
    sqlx::query(
        "INSERT INTO rh_wiki_deferred
             (summary_id, waits_session, waits_conversation, applied_topic, patches, deferred_at,
              owner_user_id)
         VALUES ($1, $2, $3, $4, $5::jsonb, $6, (SELECT owner_user_id FROM rh_summaries WHERE id = $1))
         ON CONFLICT (summary_id) DO UPDATE SET waits_session = EXCLUDED.waits_session,
            waits_conversation = EXCLUDED.waits_conversation, applied_topic = EXCLUDED.applied_topic,
            patches = EXCLUDED.patches, deferred_at = EXCLUDED.deferred_at",
    )
    .bind(deferred.summary_id)
    .bind(deferred.waits.session().map(SessionId::as_str))
    .bind(deferred.waits.conversation())
    .bind(deferred.applied.map(TopicId::uuid))
    .bind(encoded(&deferred.patches)?)
    .bind(deferred.at)
    .execute(&mut **tx)
    .await
    .map_err(query_error)?;
    Ok(())
}
