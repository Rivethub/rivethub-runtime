use async_trait::async_trait;
use jiff::Timestamp;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::consolidate::{WikiConsolidateStore, WikiFold, WikiTopicStats};
use kernel::wiki::page::{FoldMark, FoldedTopic, WikiPage, fold_page, fold_text};
use sqlx::{Postgres, Transaction};

use super::{lock_slug, query_error, settle, topics};
use crate::PostgresMemory;
use crate::wiki_writer::{LockedRepository, WikiRepository};

#[derive(Debug, sqlx::FromRow)]
struct StatsRow {
    slug: String,
    title: String,
    citations: i64,
    history_count: i32,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    created_at: Timestamp,
    bound: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct TextRow {
    title: String,
    current_state: String,
    article: String,
}

#[async_trait]
impl WikiConsolidateStore for PostgresMemory {
    async fn wiki_topic_stats(&self) -> Result<Vec<WikiTopicStats>, WikiStoreError> {
        let rows = sqlx::query_as::<_, StatsRow>(
            "SELECT t.slug, t.title, t.history_count, t.created_at, t.topic_id IS NOT NULL AS bound,
                    (SELECT count(*) FROM rh_wiki_citations c WHERE c.topic_slug = t.slug) AS citations
               FROM rh_wiki_topics t
              WHERE t.scope IS NULL OR t.scope = (SELECT s.scope FROM rh_wiki_scopes s WHERE s.directory = '')
              ORDER BY t.slug",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(query_error)?;
        rows.into_iter()
            .map(|row| {
                Ok(WikiTopicStats {
                    slug: row.slug,
                    title: row.title,
                    citations: count(row.citations)?,
                    history: count(row.history_count.into())?,
                    created_at: row.created_at,
                    bound: row.bound,
                })
            })
            .collect()
    }

    async fn fold_wiki(
        &self,
        fold: &WikiFold,
        now: Timestamp,
    ) -> Result<Vec<String>, WikiStoreError> {
        let directory = self
            .wiki_directory
            .as_ref()
            .ok_or(WikiStoreError::DirectoryMissing)?;
        let (folded, released) = {
            let mut repository = WikiRepository::for_store(directory.clone()).lock().await?;
            let folded = self.fold_pages(&mut repository, fold, now).await;
            (folded, repository.release().await)
        };
        settle(folded, released)
    }
}

impl PostgresMemory {
    async fn fold_pages(
        &self,
        repository: &mut LockedRepository,
        fold: &WikiFold,
        now: Timestamp,
    ) -> Result<Vec<String>, WikiStoreError> {
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        let mut slugs: Vec<&str> = std::iter::once(fold.parent.as_str())
            .chain(fold.children.iter().map(String::as_str))
            .collect();
        slugs.sort_unstable();
        slugs.dedup();
        let keys: Vec<&str> = slugs.clone();
        let ids: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT summary_id FROM rh_wiki_citations WHERE topic_slug = ANY($1)
             UNION SELECT source_id FROM rh_wiki_provenance
              WHERE topic_slug = ANY($1) AND source_kind = 'summary'",
        )
        .bind(&keys)
        .fetch_all(&mut *tx)
        .await
        .map_err(query_error)?;
        let live = crate::summary_replace::shared_visible(&mut tx, &ids)
            .await
            .map_err(super::fence_error)?;
        let live: Vec<_> = live.into_iter().map(|input| input.id).collect();
        for slug in slugs {
            lock_slug(&mut tx, slug).await?;
        }
        if text(&mut tx, &fold.parent).await?.is_none() {
            return Err(WikiStoreError::TopicMissing(fold.parent.clone()));
        }
        let Some(mut page) = repository.read(&fold.parent).await? else {
            tracing::warn!("wiki consolidate skipped {}: no page file", fold.parent);
            return Ok(Vec::new());
        };
        let mut folded = Vec::new();
        for child in fold.children.iter().filter(|child| **child != fold.parent) {
            let Some(row) = text(&mut tx, child).await? else {
                continue;
            };
            absorb(
                &mut page,
                child,
                row,
                repository.read(child).await?,
                fold.mark,
            );
            folded.push(child.clone());
        }
        if folded.is_empty() {
            tx.commit().await.map_err(query_error)?;
            return Ok(folded);
        }
        let message = format!(
            "wiki({}): consolidate {}\n\nPipeline: wiki-v7",
            fold.parent,
            folded.join(", ")
        );
        let git_sha = repository.fold(&mut page, &folded, &message, now).await?;
        topics::store_topic(&mut tx, &page, &git_sha, now).await?;
        for child in &folded {
            retire(&mut tx, child, &fold.parent, now, &live).await?;
        }
        tx.commit().await.map_err(query_error)?;
        Ok(folded)
    }
}

fn absorb(page: &mut WikiPage, slug: &str, row: TextRow, child: Option<WikiPage>, mark: FoldMark) {
    if let Some(child) = child {
        fold_page(page, child, mark);
        return;
    }
    if !page.meta.aliases.iter().any(|alias| alias == slug) {
        page.meta.aliases.push(slug.to_owned());
    }
    fold_text(
        &mut page.current_state,
        &mut page.article,
        FoldedTopic {
            slug: slug.to_owned(),
            title: row.title,
            current_state: row.current_state,
            article: row.article,
        },
        mark,
    );
}

async fn text(
    tx: &mut Transaction<'_, Postgres>,
    slug: &str,
) -> Result<Option<TextRow>, WikiStoreError> {
    sqlx::query_as::<_, TextRow>(
        "SELECT title, current_state, article FROM rh_wiki_topics WHERE slug = $1",
    )
    .bind(slug)
    .fetch_optional(&mut **tx)
    .await
    .map_err(query_error)
}

async fn retire(
    tx: &mut Transaction<'_, Postgres>,
    child: &str,
    parent: &str,
    now: Timestamp,
    live: &[uuid::Uuid],
) -> Result<(), WikiStoreError> {
    for statement in [
        "INSERT INTO rh_wiki_citations (topic_slug, summary_id, kind, note, cited_at, owner_user_id)
         SELECT $2, summary_id, kind, note, cited_at, owner_user_id
           FROM rh_wiki_citations WHERE topic_slug = $1 AND summary_id = ANY($3)
         ON CONFLICT DO NOTHING",
        "INSERT INTO rh_wiki_provenance
             (topic_slug, source_kind, source_id, conversation_id, git_sha, created_at, owner_user_id)
         SELECT $2, source_kind, source_id, conversation_id, git_sha, created_at, owner_user_id
           FROM rh_wiki_provenance WHERE topic_slug = $1 AND (source_kind <> 'summary' OR source_id = ANY($3))
         ON CONFLICT DO NOTHING",
    ] {
        sqlx::query(statement)
            .bind(child)
            .bind(parent)
            .bind(live)
            .execute(&mut **tx)
            .await
            .map_err(query_error)?;
    }
    sqlx::query("UPDATE rh_wiki_redirects SET to_slug = $2 WHERE to_slug = $1")
        .bind(child)
        .bind(parent)
        .execute(&mut **tx)
        .await
        .map_err(query_error)?;
    sqlx::query!("DELETE FROM rh_wiki_redirects WHERE from_slug = $1", parent)
        .execute(&mut **tx)
        .await
        .map_err(query_error)?;
    sqlx::query!(
        "WITH retired AS (
            DELETE FROM rh_wiki_topics WHERE slug = $1 RETURNING owner_user_id
         )
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at, owner_user_id)
         SELECT $1, $2, $3, coalesce(rh_scope_principal(), owner_user_id) FROM retired
         ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug,
             owner_user_id = coalesce(rh_wiki_redirects.owner_user_id, EXCLUDED.owner_user_id)",
        child,
        parent,
        crate::time::BindTime::bind_time(now) as _
    )
    .execute(&mut **tx)
    .await
    .map_err(query_error)?;
    Ok(())
}

fn count(value: i64) -> Result<u64, WikiStoreError> {
    u64::try_from(value).map_err(|error| WikiStoreError::Query(Box::new(error)))
}

#[cfg(test)]
#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn folding_adopts_a_legacy_redirect(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, owner_user_id)
         VALUES ('child', 'Child', 'alice'), ('parent', 'Parent', 'alice');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug) VALUES ('child', 'parent')",
    )
    .execute(&mut *tx)
    .await?;
    retire(&mut tx, "child", "parent", Timestamp::now(), &[]).await?;
    let owner: Option<String> =
        sqlx::query_scalar("SELECT owner_user_id FROM rh_wiki_redirects WHERE from_slug = 'child'")
            .fetch_one(&mut *tx)
            .await?;
    assert_eq!(owner.as_deref(), Some("alice"));
    Ok(())
}
