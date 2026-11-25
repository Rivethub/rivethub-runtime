use jiff::Timestamp;
use sqlx::Postgres;
use sqlx::Transaction;

use super::bind::PageKey;
use super::bind::page_key;
use super::error::query_error;
use super::error::store_error;
use super::lease::entity_home;
use super::merge::TopicSide;
use crate::PostgresMemory;
use crate::wiki_scope::WikiScope;
use kernel::wiki::entity_pass::EntityStoreError;

#[derive(Debug, sqlx::FromRow)]
pub(super) struct PageFile {
    pub(super) directory: String,
    pub(super) local: String,
}

async fn bound_page(
    tx: &mut Transaction<'_, Postgres>,
    topic: uuid::Uuid,
) -> Result<Option<PageFile>, sqlx::Error> {
    sqlx::query_as::<_, PageFile>(
        "SELECT coalesce(s.directory, '') AS directory,
                coalesce(t.local_slug, t.slug) AS local
           FROM rh_wiki_topics t
           LEFT JOIN rh_wiki_scopes s ON s.scope = t.scope
          WHERE t.topic_id = $1",
    )
    .bind(topic)
    .fetch_optional(&mut **tx)
    .await
}

async fn absorbed_page(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
) -> Result<Option<PageFile>, sqlx::Error> {
    sqlx::query_as::<_, PageFile>(
        "SELECT coalesce(s.directory, '') AS directory,
                coalesce(t.local_slug, t.slug) AS local
           FROM rh_wiki_topics t
           LEFT JOIN rh_wiki_scopes s ON s.scope = t.scope
          WHERE t.slug = $1",
    )
    .bind(key)
    .fetch_optional(&mut **tx)
    .await
}

pub(super) async fn redirect_wiki(
    tx: &mut Transaction<'_, Postgres>,
    survivor: &TopicSide,
    absorbed: &TopicSide,
    primary: Option<&str>,
    now: Timestamp,
) -> Result<Option<PageFile>, EntityStoreError> {
    let home = entity_home(tx, survivor.id, primary).await?;
    let into = page_key(tx, survivor.id, &survivor.slug, &home).await?;
    let absorbed_home = entity_home(tx, absorbed.id, primary).await?;
    let bound = bound_page(tx, absorbed.id).await.map_err(query_error)?;
    let from = page_key(tx, absorbed.id, &absorbed.slug, &absorbed_home).await?;
    let removed = match bound {
        Some(file) => Some(file),
        None => absorbed_page(tx, &from.key).await.map_err(query_error)?,
    };
    ensure_page(tx, survivor, &into, &home)
        .await
        .map_err(query_error)?;
    move_page(tx, &from.key, &into.key, &absorbed.owner_user_id, now)
        .await
        .map_err(query_error)?;
    Ok(removed)
}

async fn move_page(
    tx: &mut Transaction<'_, Postgres>,
    from: &str,
    into: &str,
    owner: &str,
    now: Timestamp,
) -> Result<(), sqlx::Error> {
    copy_sources(tx, from, into).await?;
    sqlx::query("UPDATE rh_wiki_redirects SET to_slug = $2 WHERE to_slug = $1 AND from_slug <> $2")
        .bind(from)
        .bind(into)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM rh_wiki_topics WHERE slug = $1")
        .bind(from)
        .execute(&mut **tx)
        .await?;
    sqlx::query!(
        "INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at, owner_user_id)
         VALUES ($1, $2, $3, coalesce(rh_scope_principal(), nullif($4, '')))
         ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug,
             created_at = EXCLUDED.created_at,
             owner_user_id = coalesce(rh_wiki_redirects.owner_user_id, EXCLUDED.owner_user_id)",
        from,
        into,
        crate::time::BindTime::bind_time(now) as _,
        owner
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn ensure_page(
    tx: &mut Transaction<'_, Postgres>,
    survivor: &TopicSide,
    page: &PageKey,
    home: &WikiScope,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_wiki_topics (slug, title, topic_id, owner_user_id, scope, local_slug, visibility)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (slug) DO NOTHING",
    )
    .bind(&page.key)
    .bind(&survivor.name)
    .bind(survivor.id)
    .bind(home.row_owner())
    .bind(&home.scope)
    .bind(&page.local)
    .bind(&home.visibility)
    .execute(&mut **tx)
    .await?;
    super::bind::bind_page(tx, survivor.id, &page.key).await
}

async fn copy_sources(
    tx: &mut Transaction<'_, Postgres>,
    from_slug: &str,
    into_slug: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(COPY_CITATIONS)
        .bind(from_slug)
        .bind(into_slug)
        .execute(&mut **tx)
        .await?;
    sqlx::query(COPY_PROVENANCE)
        .bind(from_slug)
        .bind(into_slug)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(super) async fn remove_page_file(
    memory: &PostgresMemory,
    page: &PageFile,
) -> Result<(), EntityStoreError> {
    let Some(root) = memory.wiki_directory.clone() else {
        return Ok(());
    };
    let directory = if page.directory.is_empty() {
        root
    } else {
        root.join(&page.directory)
    };
    let (removed, released) = {
        let mut repository = crate::wiki_writer::WikiRepository::new(directory)
            .lock()
            .await
            .map_err(write_error)?;
        let message = format!("entity merge: remove {}", page.local);
        let removed = repository
            .remove(std::slice::from_ref(&page.local), &message)
            .await;
        (removed, repository.release().await)
    };
    removed.map_err(write_error)?;
    released.map_err(write_error)
}

fn write_error(error: kernel::wiki::builder::WikiWriteError) -> EntityStoreError {
    store_error(kernel::wiki::builder::WikiStoreError::Page(error))
}

const COPY_CITATIONS: &str = "\
INSERT INTO rh_wiki_citations (topic_slug, summary_id, kind, note, cited_at, owner_user_id)
SELECT $2, summary_id, kind, note, cited_at, owner_user_id
  FROM rh_wiki_citations WHERE topic_slug = $1
ON CONFLICT DO NOTHING";

const COPY_PROVENANCE: &str = "\
INSERT INTO rh_wiki_provenance
    (topic_slug, source_kind, source_id, conversation_id, git_sha, created_at, owner_user_id)
SELECT $2, source_kind, source_id, conversation_id, git_sha, created_at, owner_user_id
  FROM rh_wiki_provenance WHERE topic_slug = $1
ON CONFLICT DO NOTHING";
