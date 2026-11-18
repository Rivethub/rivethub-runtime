use async_trait::async_trait;
use chrono::{DateTime, Utc};
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::recompile::{
    RecompileSource, Recompiled, SourceSummary, WikiRecompileStore, rebase_recompiled,
};
use uuid::Uuid;

use super::overlays::Placed;
use super::{lock_slug, query_error, settle, topics};
use crate::PostgresMemory;
use crate::wiki_scope::{page_home, scope_named};
use crate::wiki_writer::{LockedRepository, WikiRepository};

const SUMMARIES: i64 = 400;

#[derive(Debug, sqlx::FromRow)]
struct SummaryRow {
    id: Uuid,
    topic_id: Option<Uuid>,
}

#[async_trait]
impl WikiRecompileStore for PostgresMemory {
    async fn recompile_source(
        &self,
        slug: &WikiSlug,
        since: DateTime<Utc>,
    ) -> Result<Option<RecompileSource>, WikiStoreError> {
        let placed = self.recompile_home(slug).await?;
        let repository = WikiRepository::new(self.directory_of(&placed)?);
        let local = WikiSlug::try_from(placed.local.clone())
            .map_err(|error| WikiStoreError::InvalidPatch(error.to_string()))?;
        let Some(mut page) = repository.read_unlocked(&local).await? else {
            return Ok(None);
        };
        let history = repository.history_since(&local, since).await?;
        page.meta.slug = slug.clone();
        let mut conn = self.pool.acquire().await.map_err(query_error)?;
        let rows = sqlx::query_as::<_, SummaryRow>(
            "SELECT s.id, p.topic_id
               FROM rh_wiki_citations w
               JOIN rh_summaries s ON s.id = w.summary_id
               LEFT JOIN rh_conversations c ON c.id = s.conversation_id
               LEFT JOIN rh_session_placements p ON p.session_id = c.session_key
              WHERE w.topic_slug = $1
              ORDER BY s.created_at DESC, s.id
              LIMIT $2",
        )
        .bind(slug.as_ref())
        .bind(SUMMARIES)
        .fetch_all(&mut *conn)
        .await
        .map_err(query_error)?;
        let mut summaries = Vec::with_capacity(rows.len());
        for row in rows {
            let job = kernel::wiki::builder::ExtractWiki {
                summary_id: row.id,
                conversation_id: None,
            };
            let input = match super::load_summary(&mut conn, &job, false).await {
                Ok(input) => input,
                Err(error) if error.obsolete_input() => continue,
                Err(error) => return Err(error),
            };
            summaries.push(SourceSummary {
                topic: row.topic_id.map(|topic| topic.to_string()),
                at: input.created_at.unwrap_or(since),
                content: input.content.clone(),
                input: Some(input),
            });
        }
        Ok(Some(RecompileSource {
            page,
            history,
            summaries,
        }))
    }

    async fn store_recompiled(
        &self,
        recompiled: Recompiled,
        now: DateTime<Utc>,
    ) -> Result<String, WikiStoreError> {
        let placed = self.recompile_home(&recompiled.page.meta.slug).await?;
        let mut repository = WikiRepository::new(self.directory_of(&placed)?)
            .lock()
            .await?;
        let stored = self
            .store_locked(&mut repository, recompiled, &placed, now)
            .await;
        settle(stored, repository.release().await)
    }
}

impl PostgresMemory {
    fn directory_of(&self, placed: &Placed) -> Result<std::path::PathBuf, WikiStoreError> {
        let directory = placed
            .scope
            .as_ref()
            .map_or(String::new(), |scope| scope.directory.clone());
        self.wiki_home(&directory)
    }

    async fn recompile_home(&self, slug: &WikiSlug) -> Result<Placed, WikiStoreError> {
        let mut conn = self.pool.acquire().await.map_err(query_error)?;
        let home = page_home(&mut conn, slug.as_ref()).await?;
        let scope = match &home.scope {
            Some(name) => scope_named(&mut conn, name).await?,
            None => None,
        };
        Ok(Placed {
            scope,
            key: slug.as_ref().to_owned(),
            local: home.local,
            canonical: true,
            bind: None,
        })
    }

    async fn store_locked(
        &self,
        repository: &mut LockedRepository,
        recompiled: Recompiled,
        placed: &Placed,
        now: DateTime<Utc>,
    ) -> Result<String, WikiStoreError> {
        let Recompiled {
            inputs,
            base,
            mut page,
            mut archived,
            message,
        } = recompiled;
        let slug = placed.key.clone();
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        let ids: Vec<_> = inputs.iter().map(|input| input.id).collect();
        let current = crate::summary_replace::shared(&mut tx, &ids)
            .await
            .map_err(super::fence_error)?;
        for input in &inputs {
            super::validate_input(&current, input)?;
        }
        lock_slug(&mut tx, &slug).await?;
        let current = repository.read(&placed.local).await?.map(|mut current| {
            current.meta.slug = page.meta.slug.clone();
            current
        });
        let Some(current) = current else {
            return Err(WikiStoreError::InvalidPatch(format!(
                "wiki page {slug} was removed while it was recompiled"
            )));
        };
        if current != base {
            page = rebase_recompiled(&base, page, &current, &mut archived);
        }
        page.meta.slug = WikiSlug::try_from(placed.local.clone())
            .map_err(|error| WikiStoreError::InvalidPatch(error.to_string()))?;
        let git_sha = repository
            .recompile(&mut page, archived, &message, now)
            .await?;
        topics::store(&mut tx, &page, &slug, placed.scope.as_ref(), &git_sha, now).await?;
        tx.commit().await.map_err(query_error)?;
        Ok(git_sha)
    }
}
