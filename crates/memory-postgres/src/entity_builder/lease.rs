use std::path::Path;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::wiki::entity_pass::AppliedMark;
use kernel::wiki::entity_pass::ENTITY_LOCK_CLASS;
use kernel::wiki::entity_pass::EntityLease;
use kernel::wiki::entity_pass::EntityPage;
use kernel::wiki::entity_pass::EntityProse;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::MentionNote;
use kernel::wiki::entity_pass::PreparedEntity;
use kernel::wiki::entity_pass::PublishOutcome;
use kernel::wiki::entity_pass::StoredRewrite;
use kernel::wiki::entity_pass::prose_article;
use sqlx::Connection;
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::Transaction;
use uuid::Uuid;

use crate::wiki_writer::WikiRepository;

use super::error::query_error;

use super::error::store_error;
use super::rewrite::advance_mark_at;
use super::rewrite::applied_mark;
use super::rewrite::chosen_prose;
use super::rewrite::mark_is_ahead;
use super::rewrite::upsert_history;
use super::rewrite::write_rewrite;
use super::types::observed_types;
use super::view::load_view;
use super::view::page_from;

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        untyped,
        reason = "tool arguments and model JSON are arbitrary objects"
    )
)]
pub(super) fn hold_payload(prepared: &PreparedEntity) -> serde_json::Value {
    serde_json::json!({
        "name": prepared.name,
        "type": prepared.type_name,
        "aliases": prepared.aliases,
        "entity_ref": prepared.entity_ref,
        "facts": prepared
            .facts
            .iter()
            .map(|fact| serde_json::json!({
                "attribute": fact.attribute,
                "value": fact.value,
            }))
            .collect::<Vec<_>>(),
    })
}

pub(super) struct PgLease {
    pub(super) conn: Option<PgConnection>,
    pub(super) id: Uuid,
    pub(super) directory: PathBuf,
}

#[async_trait]
impl EntityLease for PgLease {
    async fn page(&mut self) -> Result<EntityPage, EntityStoreError> {
        let conn = held(&mut self.conn)?;
        let mut tx = conn.begin().await.map_err(query_error)?;
        let current = super::claims::current_facts(&mut tx, self.id).await?;
        let view = load_view(&mut tx, self.id).await?;
        let prose = load_page_prose(&mut tx, &view).await?;
        let observed_types = observed_types(&mut tx, self.id).await?;
        tx.commit().await.map_err(query_error)?;
        Ok(EntityPage {
            summary: prose.summary,
            article: prose_article(&prose.article),
            facts: current.facts,
            observed_types,
            fixed: current.fixed,
        })
    }

    async fn applied_through(&mut self) -> Result<Option<AppliedMark>, EntityStoreError> {
        let conn = held(&mut self.conn)?;
        applied_mark(&mut *conn, self.id).await
    }

    async fn apply(
        &mut self,
        body: &StoredRewrite,
        now: DateTime<Utc>,
    ) -> Result<PublishOutcome, EntityStoreError> {
        if body.owner.trim().is_empty() {
            return Err(EntityStoreError::Query(Box::new(std::io::Error::other(
                "entity rewrite has no owner",
            ))));
        }
        let conn = held(&mut self.conn)?;
        let mut tx = conn.begin().await.map_err(query_error)?;
        let view = load_view(&mut tx, self.id).await?;
        let older = mark_is_ahead(&mut tx, view.id, body).await?;
        write_rewrite(&mut tx, view.id, body, older).await?;
        if let Some(line) = &body.history {
            upsert_history(&mut tx, view.id, line, &body.owner).await?;
        }
        let fresh = load_view(&mut tx, self.id).await?;
        let prose = chosen_prose(&mut tx, &view, body, older).await?;
        commit_page(tx, &self.directory, &fresh, &prose, now).await
    }

    async fn record_mention(
        &mut self,
        note: &MentionNote,
        now: DateTime<Utc>,
    ) -> Result<Option<String>, EntityStoreError> {
        if note.owner.trim().is_empty() {
            return Err(EntityStoreError::Query(Box::new(std::io::Error::other(
                "entity mention has no owner",
            ))));
        }
        let conn = held(&mut self.conn)?;
        let mut tx = conn.begin().await.map_err(query_error)?;
        let view = load_view(&mut tx, self.id).await?;
        if let Some(line) = &note.history {
            upsert_history(&mut tx, view.id, line, &note.owner).await?;
        }
        advance_mark_at(
            &mut tx,
            view.id,
            note.session_at,
            note.session_id,
            &note.owner,
        )
        .await?;
        if note.history.is_none() {
            tx.commit().await.map_err(query_error)?;
            return Ok(None);
        }
        let fresh = load_view(&mut tx, self.id).await?;
        let stored = load_page_prose(&mut tx, &fresh).await?;
        let prose = EntityProse {
            summary: stored.summary,
            article: prose_article(&stored.article),
        };
        match commit_page(tx, &self.directory, &fresh, &prose, now).await? {
            PublishOutcome::Stored { git_sha, .. } => Ok(Some(git_sha)),
            PublishOutcome::Stale(_) => Ok(None),
        }
    }

    async fn release(&mut self) -> Result<(), EntityStoreError> {
        let Some(mut conn) = self.conn.take() else {
            return Ok(());
        };
        let key = format!("id:{}", self.id);
        let unlocked = sqlx::query("SELECT pg_advisory_unlock($1, hashtext($2))")
            .bind(ENTITY_LOCK_CLASS)
            .bind(key)
            .execute(&mut conn)
            .await;
        drop(conn);
        if let Err(error) = unlocked {
            tracing::warn!(%error, "entity lock unlock failed; closing the session releases it");
        }
        Ok(())
    }
}

pub(super) struct PageText {
    pub(super) summary: String,
    pub(super) article: String,
}

pub(super) fn held(conn: &mut Option<PgConnection>) -> Result<&mut PgConnection, EntityStoreError> {
    conn.as_mut().ok_or_else(|| {
        EntityStoreError::Query(Box::new(std::io::Error::other("entity lock is not held")))
    })
}

pub(super) async fn ignore_idle_timeout(conn: &mut PgConnection) {
    if let Err(error) = sqlx::query("SELECT set_config('idle_session_timeout', '0', false)")
        .execute(&mut *conn)
        .await
    {
        tracing::warn!(%error, "entity lease kept the default idle_session_timeout");
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ProseRow {
    current_state: String,
    article: String,
}

pub(super) async fn load_page_prose(
    tx: &mut Transaction<'_, Postgres>,
    view: &EntityView,
) -> Result<PageText, EntityStoreError> {
    let row = sqlx::query_as::<_, ProseRow>(
        "SELECT current_state, article FROM rh_wiki_topics
          WHERE topic_id = $1 OR slug = $2 OR local_slug = $2
          ORDER BY (topic_id = $1) DESC, updated_at DESC
          LIMIT 1",
    )
    .bind(view.id)
    .bind(&view.slug)
    .fetch_optional(&mut **tx)
    .await
    .map_err(query_error)?;
    Ok(row.map_or_else(PageText::empty, PageText::from_row))
}

impl PageText {
    fn empty() -> Self {
        Self {
            summary: String::new(),
            article: String::new(),
        }
    }

    fn from_row(row: ProseRow) -> Self {
        Self {
            summary: row.current_state,
            article: row.article,
        }
    }
}

pub(super) async fn commit_page(
    mut tx: Transaction<'_, Postgres>,
    directory: &Path,
    view: &EntityView,
    prose: &EntityProse,
    now: DateTime<Utc>,
) -> Result<PublishOutcome, EntityStoreError> {
    let mut page = page_from(view, prose)?;
    let message = format!("entity {}", view.slug);
    let (sha, released) = {
        let mut repository = WikiRepository::new(directory.to_path_buf()).lock().await?;
        let folded = repository.fold(&mut page, &[], &message, now).await?;
        crate::wiki_builder::store_wiki_page(&mut tx, &page, &view.slug, None, &folded, now)
            .await
            .map_err(store_error)?;
        super::bind::bind_page(&mut tx, view.id, &view.slug)
            .await
            .map_err(query_error)?;
        tx.commit().await.map_err(query_error)?;
        (folded, repository.release().await)
    };
    released?;
    Ok(PublishOutcome::Stored {
        git_sha: sha,
        slug: view.slug.clone(),
    })
}
