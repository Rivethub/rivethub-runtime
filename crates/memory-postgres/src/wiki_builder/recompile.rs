use async_trait::async_trait;
use jiff::Timestamp;
use kernel::tenancy::Scope;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::recompile::{
    RecompileSource, Recompiled, SourceSummary, WikiRecompileStore, rebase_recompiled,
};
use uuid::Uuid;

use super::overlays::Placed;
use super::{lock_slug, query_error, settle, topics};
use crate::PostgresMemory;
use crate::wiki_scope::{LegacyOwner, PageHome, page_home, page_place, primary_scope, scope_named};
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
        since: Timestamp,
    ) -> Result<Option<RecompileSource>, WikiStoreError> {
        let Some(placed) = self.recompile_home(slug).await? else {
            return Ok(None);
        };
        let repository = WikiRepository::for_store(self.directory_of(&placed)?);
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
        now: Timestamp,
    ) -> Result<String, WikiStoreError> {
        let slug = &recompiled.page.meta.slug;
        let placed = self
            .recompile_home(slug)
            .await?
            .ok_or_else(|| WikiStoreError::TopicMissing(slug.as_ref().to_owned()))?;
        let mut repository = WikiRepository::for_store(self.directory_of(&placed)?)
            .lock()
            .await?;
        let stored = self
            .store_locked(&mut repository, recompiled, &placed, now)
            .await;
        settle(stored, repository.release().await)
    }
}

impl PostgresMemory {
    pub(super) fn directory_of(
        &self,
        placed: &Placed,
    ) -> Result<std::path::PathBuf, WikiStoreError> {
        let directory = placed
            .scope
            .as_ref()
            .map_or(String::new(), |scope| scope.directory.clone());
        self.wiki_home(&directory)
    }

    pub async fn primary_page_key(
        &self,
        local: &WikiSlug,
    ) -> Result<Option<WikiSlug>, WikiStoreError> {
        let legacy = self.legacy_owner().await?;
        let mut conn = self.pool.acquire().await.map_err(query_error)?;
        let keys = sqlx::query_scalar::<_, String>(
            "SELECT slug FROM rh_wiki_topics WHERE coalesce(local_slug, slug) = $1 ORDER BY slug",
        )
        .bind(local.as_ref())
        .fetch_all(&mut *conn)
        .await
        .map_err(query_error)?;
        for key in keys {
            let home = page_home(&mut conn, &key, &legacy).await?;
            if home.is_some_and(|home| home.directory.is_empty()) {
                return WikiSlug::try_from(key)
                    .map(Some)
                    .map_err(|error| WikiStoreError::InvalidPatch(error.to_string()));
            }
        }
        Ok(page_place(&mut conn, local.as_ref())
            .await?
            .is_none()
            .then(|| local.clone()))
    }

    async fn recompile_home(&self, slug: &WikiSlug) -> Result<Option<Placed>, WikiStoreError> {
        let legacy = self.legacy_owner().await?;
        let mut conn = self.pool.acquire().await.map_err(query_error)?;
        let (scope, local) = match page_home(&mut conn, slug.as_ref(), &legacy).await? {
            Some(PageHome {
                scope: Some(name),
                local,
                ..
            }) => (scope_named(&mut conn, &name).await?, local),
            Some(home) => (None, home.local),
            None if matches!(Scope::current(), Some(Scope::Maintenance))
                && page_place(&mut conn, slug.as_ref()).await?.is_none() =>
            {
                let scope = match legacy {
                    LegacyOwner::Unknown => None,
                    LegacyOwner::Owner(_) | LegacyOwner::Contested => {
                        let Some(primary) = primary_scope(&mut conn, &legacy).await? else {
                            tracing::warn!(
                                "wiki recompile skipped {}: the primary wiki owner is contested or has no primary scope",
                                slug.as_ref()
                            );
                            return Ok(None);
                        };
                        Some(primary)
                    }
                };
                (scope, slug.as_ref().to_owned())
            }
            None => return Ok(None),
        };
        Ok(Some(Placed {
            scope,
            key: slug.as_ref().to_owned(),
            local,
            canonical: true,
            fresh: false,
            bind: None,
        }))
    }

    async fn store_locked(
        &self,
        repository: &mut LockedRepository,
        recompiled: Recompiled,
        placed: &Placed,
        now: Timestamp,
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
