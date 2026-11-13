use chrono::DateTime;
use chrono::Utc;
use kernel::wiki::entity_pass::AppliedMark;
use kernel::wiki::entity_pass::EntityProse;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::PreparedEntity;
use kernel::wiki::entity_pass::PreparedHistory;
use kernel::wiki::entity_pass::StoredRewrite;
use kernel::wiki::entity_pass::older_than_mark;
use kernel::wiki::entity_pass::prose_article;
use sqlx::Postgres;
use sqlx::Transaction;
use uuid::Uuid;

use super::error::query_error;

use super::lease::load_page_prose;
pub(super) async fn write_history(
    tx: &mut Transaction<'_, Postgres>,
    entity: Uuid,
    prepared: &PreparedEntity,
    owner: &str,
) -> Result<(), EntityStoreError> {
    for line in &prepared.history {
        sqlx::query(
            "INSERT INTO rh_entity_history
                (entity_id, summary_id, conversation_id, session_on, sentence, owner_user_id)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (entity_id, summary_id) DO NOTHING",
        )
        .bind(entity)
        .bind(line.summary_id)
        .bind(line.conversation_id)
        .bind(line.session_on)
        .bind(&line.sentence)
        .bind(owner)
        .execute(&mut **tx)
        .await
        .map_err(query_error)?;
    }
    Ok(())
}

pub(super) async fn mark_is_ahead(
    tx: &mut Transaction<'_, Postgres>,
    entity: Uuid,
    body: &StoredRewrite,
) -> Result<bool, EntityStoreError> {
    let mark = applied_mark(&mut **tx, entity).await?;
    Ok(mark.is_some_and(|mark| older_than_mark(body.session_at, body.session_id, mark)))
}

pub(super) async fn write_rewrite(
    tx: &mut Transaction<'_, Postgres>,
    entity: Uuid,
    body: &StoredRewrite,
    older: bool,
) -> Result<(), EntityStoreError> {
    super::claims::assert_snapshot(tx, entity, &body.facts, &body.evidence, &body.owner).await?;
    if older {
        return Ok(());
    }
    advance_mark_at(tx, entity, body.session_at, body.session_id, &body.owner).await
}

pub(super) async fn chosen_prose(
    tx: &mut Transaction<'_, Postgres>,
    view: &EntityView,
    body: &StoredRewrite,
    older: bool,
) -> Result<EntityProse, EntityStoreError> {
    if !older {
        return Ok(EntityProse {
            summary: body.summary.clone(),
            article: body.article.clone(),
        });
    }
    let stored = load_page_prose(tx, view).await?;
    Ok(EntityProse {
        summary: stored.summary,
        article: prose_article(&stored.article),
    })
}

pub(super) async fn advance_mark_at(
    tx: &mut Transaction<'_, Postgres>,
    entity: Uuid,
    session_at: DateTime<Utc>,
    session_id: Uuid,
    owner: &str,
) -> Result<(), EntityStoreError> {
    sqlx::query(
        "INSERT INTO rh_entity_applied (entity_id, applied_through, summary_id, owner_user_id)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (entity_id) DO UPDATE SET
            applied_through = EXCLUDED.applied_through,
            summary_id = EXCLUDED.summary_id,
            owner_user_id = EXCLUDED.owner_user_id
          WHERE rh_entity_applied.applied_through < EXCLUDED.applied_through
             OR (rh_entity_applied.applied_through = EXCLUDED.applied_through
                 AND rh_entity_applied.summary_id < EXCLUDED.summary_id)",
    )
    .bind(entity)
    .bind(session_at)
    .bind(session_id)
    .bind(owner)
    .execute(&mut **tx)
    .await
    .map_err(query_error)?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct MarkRow {
    applied_through: DateTime<Utc>,
    summary_id: Uuid,
}

pub(super) async fn upsert_history(
    tx: &mut Transaction<'_, Postgres>,
    entity: Uuid,
    line: &PreparedHistory,
    owner: &str,
) -> Result<(), EntityStoreError> {
    sqlx::query(
        "INSERT INTO rh_entity_history
            (entity_id, summary_id, conversation_id, session_on, sentence, owner_user_id)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (entity_id, summary_id) DO UPDATE SET sentence = EXCLUDED.sentence",
    )
    .bind(entity)
    .bind(line.summary_id)
    .bind(line.conversation_id)
    .bind(line.session_on)
    .bind(&line.sentence)
    .bind(owner)
    .execute(&mut **tx)
    .await
    .map_err(query_error)?;
    Ok(())
}

pub(super) async fn applied_mark<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    entity: Uuid,
) -> Result<Option<AppliedMark>, EntityStoreError> {
    let row = sqlx::query_as::<_, MarkRow>(
        "SELECT applied_through, summary_id FROM rh_entity_applied WHERE entity_id = $1",
    )
    .bind(entity)
    .fetch_optional(executor)
    .await
    .map_err(query_error)?;
    Ok(row.map(|row| AppliedMark {
        at: row.applied_through,
        summary_id: row.summary_id,
    }))
}
