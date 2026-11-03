mod coalesce;
mod consolidate;
mod gate;
mod held;
mod overlays;
mod recompile;
mod topic_pages;
mod topics;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use kernel::owner::Owner;
use kernel::topics::Slug;
use kernel::wiki::builder::{
    BACKFILL_PRIORITY, BackfillBudget, DeferredWiki, ExtractWiki, HeldEntity, RouteLease,
    WIKI_PIPELINE_VERSION, WikiBuilderStore, WikiRouting, WikiStoreError, WikiSummary, WikiWork,
    WikiWriteError,
};
use kernel::wiki::page::WikiPatch;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::normalize_slug;
use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

use crate::PostgresMemory;

#[derive(Debug, sqlx::FromRow)]
struct SummaryRow {
    id: Uuid,
    conversation_id: Option<Uuid>,
    content: String,
    session_key: Option<String>,
    owner_user_id: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct CompletedRow {
    topics_touched: Vec<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct BackfillRow {
    id: Uuid,
    conversation_id: Option<Uuid>,
}

#[async_trait]
impl WikiBuilderStore for PostgresMemory {
    async fn wiki_work(&self, job: &ExtractWiki) -> Result<WikiWork, WikiStoreError> {
        let mut connection = self.pool.acquire().await.map_err(query_error)?;
        let summary = load_summary(&mut connection, job, false).await?;
        if let Some(topics) = completed(&mut connection, job.summary_id).await? {
            return Ok(WikiWork::Complete { topics });
        }
        Ok(WikiWork::Pending(summary))
    }

    async fn wiki_backfill(
        &self,
        budget: BackfillBudget,
    ) -> Result<Vec<ExtractWiki>, WikiStoreError> {
        let rows = sqlx::query_as::<_, BackfillRow>(
            "SELECT s.id, s.conversation_id FROM rh_summaries s
             LEFT JOIN rh_conversations c ON c.id = s.conversation_id
             LEFT JOIN rh_wiki_extractions e ON e.summary_id = s.id
             WHERE s.kind = 'leaf'
               AND (c.id IS NULL OR c.session_key NOT LIKE 'heartbeat:%')
               AND (e.summary_id IS NULL OR e.status = 'failed'
                    OR (e.status = 'done' AND cardinality(e.topics_touched) > 0 AND e.git_sha IS NULL))
               AND NOT EXISTS (SELECT 1 FROM rh_flagged_jobs f WHERE f.job_key = 'wiki-ext-' || s.id::text)
               AND NOT EXISTS (SELECT 1 FROM graphile_worker._private_jobs j WHERE j.key = 'wiki-ext-' || s.id::text)
             ORDER BY s.created_at DESC, s.id
             LIMIT greatest(0, (SELECT CASE WHEN EXISTS (
                         SELECT 1 FROM graphile_worker._private_jobs n
                         JOIN graphile_worker._private_tasks t ON t.id = n.task_id
                         WHERE n.is_available AND n.run_at <= now()
                           AND ((t.identifier = 'extract-wiki' AND n.priority < $3)
                             OR (t.identifier = 'compact-conversation' AND n.priority < 0)))
                       THEN $2 ELSE $1 END)
                     - (SELECT count(*) FROM graphile_worker._private_jobs q
                         WHERE q.key LIKE 'wiki-ext-%' AND q.priority = $3))",
        )
        .bind(i64::from(budget.idle))
        .bind(i64::from(budget.busy))
        .bind(BACKFILL_PRIORITY)
        .fetch_all(&self.pool)
        .await
        .map_err(query_error)?;
        Ok(rows
            .into_iter()
            .map(|row| ExtractWiki {
                summary_id: row.id,
                conversation_id: row.conversation_id,
            })
            .collect())
    }

    async fn apply_wiki(
        &self,
        job: &ExtractWiki,
        patches: Vec<WikiPatch>,
        routing: &WikiRouting,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>, WikiStoreError> {
        let home = {
            let mut connection = self.pool.acquire().await.map_err(query_error)?;
            crate::wiki_scope::summary_scope(&mut connection, job.summary_id, self.wiki_primary())
                .await?
        };
        let directory = self.wiki_home(&home.directory)?;
        let (applied, released) = {
            let mut repository = crate::wiki_writer::WikiRepository::new(directory)
                .lock()
                .await?;
            let applied = self
                .apply_pages(
                    &mut repository,
                    Work {
                        job,
                        routing,
                        home: &home,
                    },
                    patches,
                    now,
                )
                .await;
            (applied, repository.release().await)
        };
        settle(applied, released)
    }

    async fn fail_wiki(
        &self,
        summary_id: Uuid,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        sqlx::query(
            "INSERT INTO rh_wiki_extractions (summary_id, status, pipeline_version, error, extracted_at, owner_user_id)
             VALUES ($1, 'failed', $2, $3, $4, (SELECT owner_user_id FROM rh_summaries WHERE id = $1))
             ON CONFLICT (summary_id) DO UPDATE SET status = 'failed', pipeline_version = EXCLUDED.pipeline_version,
                error = EXCLUDED.error, extracted_at = EXCLUDED.extracted_at
             WHERE rh_wiki_extractions.status = 'failed' OR rh_wiki_extractions.pipeline_version < $2
                OR (rh_wiki_extractions.status = 'done' AND rh_wiki_extractions.git_sha IS NULL AND cardinality(rh_wiki_extractions.topics_touched) > 0)",
        )
        .bind(summary_id).bind(WIKI_PIPELINE_VERSION).bind(error).bind(now)
        .execute(&self.pool).await.map_err(query_error)?;
        Ok(())
    }

    async fn hold_entity(&self, held: HeldEntity) -> Result<Vec<HeldEntity>, WikiStoreError> {
        held::hold(&self.pool, held).await
    }

    async fn held_entities(&self, key: &Slug) -> Result<Vec<HeldEntity>, WikiStoreError> {
        held::candidates(&self.pool, key).await
    }

    async fn release_entity(&self, held: &[HeldEntity]) -> Result<(), WikiStoreError> {
        held::release(&self.pool, held).await
    }

    async fn defer_wiki(&self, deferred: DeferredWiki) -> Result<(), WikiStoreError> {
        held::defer(&self.pool, deferred).await
    }

    async fn unsettled_notes(&self, limit: u32) -> Result<Vec<DeferredWiki>, WikiStoreError> {
        held::unsettled(&self.pool, limit).await
    }

    async fn cite_wiki(
        &self,
        summary_id: Uuid,
        slug: &WikiSlug,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        let placed = self.place_note_page(slug, None).await?;
        let cited = WikiSlug::try_from(placed.key)
            .map_err(|error| WikiStoreError::InvalidPatch(error.to_string()))?;
        held::cite(&self.pool, summary_id, &cited, now).await
    }

    async fn coalescable(
        &self,
        lead: Uuid,
        topic: kernel::topics::TopicId,
        limit: u32,
    ) -> Result<Vec<kernel::wiki::builder::Coalescable>, WikiStoreError> {
        coalesce::candidates(&self.pool, lead, topic, limit).await
    }

    async fn lease_route(&self, route: &str) -> Result<Option<RouteLease>, WikiStoreError> {
        coalesce::lease(&self.pool, route).await
    }

    async fn uncite_wiki(&self, summary_id: Uuid, slug: &WikiSlug) -> Result<(), WikiStoreError> {
        let placed = self.place_note_page(slug, None).await?;
        let cited = WikiSlug::try_from(placed.key)
            .map_err(|error| WikiStoreError::InvalidPatch(error.to_string()))?;
        held::uncite(&self.pool, summary_id, &cited).await
    }
}

struct Work<'a> {
    job: &'a ExtractWiki,
    routing: &'a WikiRouting,
    home: &'a crate::wiki_scope::WikiScope,
}

impl PostgresMemory {
    async fn apply_pages(
        &self,
        repository: &mut crate::wiki_writer::LockedRepository,
        work: Work<'_>,
        mut patches: Vec<WikiPatch>,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>, WikiStoreError> {
        let Work { job, routing, home } = work;
        for patch in &mut patches {
            if patch.title.trim().is_empty() || patch.summary.trim().is_empty() {
                return Err(WikiStoreError::InvalidPatch(
                    "title and summary must be nonempty".to_owned(),
                ));
            }
            patch.slug = WikiSlug::try_from(normalize_slug(patch.slug.as_ref()))
                .map_err(|error| WikiStoreError::InvalidPatch(error.to_string()))?;
        }
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        let summary = load_summary(&mut tx, job, true).await?;
        if let Some(topics) = completed(&mut tx, job.summary_id).await? {
            tx.commit().await.map_err(query_error)?;
            return Ok(topics);
        }
        lock_slugs(&mut tx, &patches, &routing.cited).await?;
        let mut cited = Vec::new();
        for slug in &routing.cited {
            if within(&mut tx, slug.as_ref(), home).await? {
                cited.push(slug.clone());
            } else if let Some(key) = overlaid(&mut tx, slug.as_ref(), home).await? {
                cited.push(
                    WikiSlug::try_from(key)
                        .map_err(|error| WikiStoreError::InvalidPatch(error.to_string()))?,
                );
            }
        }
        let topic_page = match &routing.topic_page {
            Some(page) if within(&mut tx, page.as_ref(), home).await? => Some(page),
            Some(_) | None => None,
        };
        let date: DateTime<Utc> =
            sqlx::query_scalar("SELECT created_at FROM rh_summaries WHERE id = $1")
                .bind(job.summary_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(query_error)?;
        let source = topics::Source {
            summary: &summary,
            date,
            topic_page,
            merged: &routing.merged,
            home,
            also: &routing.also,
        };
        let mut touched = Vec::new();
        let mut last_sha = None;
        for patch in patches {
            let (slug, sha) = topics::apply(&mut tx, repository, &source, patch, now).await?;
            touched.push(slug);
            last_sha = Some(sha);
        }
        for slug in &cited {
            for cited_by in std::iter::once(&summary).chain(&routing.also) {
                topics::cite(&mut tx, slug.as_ref(), cited_by, now).await?;
            }
        }
        touched.sort();
        touched.dedup();
        let mut summaries = vec![&summary];
        summaries.extend(&routing.also);
        coalesce::record(&mut tx, &summaries, &touched, last_sha.as_ref(), now).await?;
        tx.commit().await.map_err(query_error)?;
        Ok(touched)
    }
}

fn settle<T>(
    applied: Result<T, WikiStoreError>,
    released: Result<(), WikiWriteError>,
) -> Result<T, WikiStoreError> {
    match (applied, released) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
        (Err(error), Err(release)) => {
            tracing::error!(%release, "wiki writer lock release failed");
            Err(error)
        }
    }
}

async fn load_summary(
    connection: &mut PgConnection,
    job: &ExtractWiki,
    lock: bool,
) -> Result<WikiSummary, WikiStoreError> {
    let mut query = sqlx::QueryBuilder::<Postgres>::new(
        "SELECT s.id, s.conversation_id, s.content, c.session_key, s.owner_user_id, s.created_at FROM rh_summaries s
         LEFT JOIN rh_conversations c ON c.id = s.conversation_id WHERE s.id = ",
    );
    query.push_bind(job.summary_id);
    if lock {
        query.push(" FOR UPDATE OF s");
    }
    let row = query
        .build_query_as::<SummaryRow>()
        .fetch_optional(connection)
        .await
        .map_err(query_error)?
        .ok_or(WikiStoreError::Missing(job.summary_id))?;
    if job
        .conversation_id
        .is_some_and(|id| Some(id) != row.conversation_id)
    {
        return Err(WikiStoreError::ConversationMismatch(job.summary_id));
    }
    Ok(WikiSummary {
        id: row.id,
        conversation_id: row.conversation_id,
        content: row.content,
        session_key: row.session_key,
        owner: Some(
            Owner::new(row.owner_user_id)
                .map_err(|error| WikiStoreError::Query(Box::new(error)))?,
        ),
        created_at: Some(row.created_at),
    })
}

async fn completed(
    connection: &mut PgConnection,
    summary_id: Uuid,
) -> Result<Option<Vec<String>>, WikiStoreError> {
    let row = sqlx::query_as::<_, CompletedRow>(
        "SELECT topics_touched FROM rh_wiki_extractions WHERE summary_id = $1
         AND status IN ('done', 'skipped') AND pipeline_version >= $2
         AND (status = 'skipped' OR cardinality(topics_touched) = 0 OR git_sha IS NOT NULL)",
    )
    .bind(summary_id)
    .bind(WIKI_PIPELINE_VERSION)
    .fetch_optional(connection)
    .await
    .map_err(query_error)?;
    Ok(row.map(|row| row.topics_touched))
}

async fn lock_slugs(
    tx: &mut Transaction<'_, Postgres>,
    patches: &[WikiPatch],
    cited: &[WikiSlug],
) -> Result<(), WikiStoreError> {
    let mut slugs: Vec<&str> = patches
        .iter()
        .flat_map(|patch| {
            std::iter::once(patch.slug.as_ref()).chain(patch.aliases.iter().map(AsRef::as_ref))
        })
        .chain(cited.iter().map(AsRef::as_ref))
        .collect();
    slugs.sort_unstable();
    slugs.dedup();
    for slug in slugs {
        lock_slug(tx, slug).await?;
    }
    Ok(())
}

async fn lock_slug(tx: &mut Transaction<'_, Postgres>, slug: &str) -> Result<(), WikiStoreError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(slug)
        .execute(&mut **tx)
        .await
        .map_err(query_error)?;
    Ok(())
}

async fn within(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    home: &crate::wiki_scope::WikiScope,
) -> Result<bool, WikiStoreError> {
    let found = crate::wiki_scope::page_home(tx, key).await?;
    Ok(match found.scope {
        Some(scope) => scope == home.scope,
        None => home.primary(),
    })
}

async fn overlaid(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    home: &crate::wiki_scope::WikiScope,
) -> Result<Option<String>, WikiStoreError> {
    let found = crate::wiki_scope::page_home(tx, key).await?;
    crate::wiki_scope::overlay(tx, home, &found.local).await
}

fn query_error(error: sqlx::Error) -> WikiStoreError {
    WikiStoreError::Query(Box::new(error))
}
