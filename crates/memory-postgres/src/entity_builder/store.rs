use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::queue::FixedJob;
use kernel::queue::JobId;
use kernel::queue::JobName;
use kernel::wiki::builder::BackfillBudget;
use kernel::wiki::entity_match::identity_keys;
use kernel::wiki::entity_pass::CandidateQuery;
use kernel::wiki::entity_pass::ENTITY_LOCK_CLASS;
use kernel::wiki::entity_pass::ENTITY_PIPELINE_VERSION;
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityLease;
use kernel::wiki::entity_pass::EntityMerge;
use kernel::wiki::entity_pass::EntityProse;
use kernel::wiki::entity_pass::EntityStore;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::EntityWork;
use kernel::wiki::entity_pass::ExtractEntities;
use kernel::wiki::entity_pass::FollowingLeaf;
use kernel::wiki::entity_pass::MatchCandidate;
use kernel::wiki::entity_pass::MatchDecision;
use kernel::wiki::entity_pass::ObserveContext;
use kernel::wiki::entity_pass::PreparedEntity;
use kernel::wiki::entity_pass::PublishOutcome;
use kernel::wiki::entity_pass::TranscriptMessage;
use kernel::wiki::entity_pass::TypeBrief;
use kernel::wiki::entity_pass::compact_key;
use pgvector::HalfVector;
use sqlx::Connection;
use sqlx::Postgres;
use sqlx::Transaction;
use uuid::Uuid;

use crate::PostgresMemory;
use crate::graphile::GraphileQueue;

use super::error::query_error;

use super::candidates::gather;
use super::error::store_error;
use super::identity::ResolveAt;
use super::identity::resolve;
use super::identity::write_aliases;
use super::known::known_entities;
use super::lease::PgLease;
use super::lease::commit_page;
use super::lease::hold_payload;
use super::lease::ignore_idle_timeout;
use super::rewrite::write_history;
use super::sql::BACKFILL;
use super::sql::DONE;
use super::sql::FAIL;
use super::sql::FINISH;
use super::sql::FOLLOWING;
use super::sql::HOLD;
use super::sql::LEAF;
use super::sql::RECORD_DECISION;
use super::sql::REMEMBER;
use super::sql::STALE;
use super::transcript::load_transcript;
use super::types::ensure_type;
use super::types::type_paths;
use super::types::type_rows;
use super::view::load_view;

const BACKFILL_PRIORITY: i16 = 50;

#[derive(Debug, sqlx::FromRow)]
struct LeafRow {
    id: Uuid,
    conversation_id: Option<Uuid>,
    content: String,
    start_at: DateTime<Utc>,
    end_at: DateTime<Utc>,
    owner_user_id: String,
}

#[derive(Debug, sqlx::FromRow)]
struct DoneRow {
    entities_touched: Vec<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct SiblingRow {
    id: Uuid,
    conversation_id: Option<Uuid>,
    content: String,
    start_at: DateTime<Utc>,
    end_at: DateTime<Utc>,
    owner_user_id: String,
    done: bool,
    job_id: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
struct JobRow {
    id: Uuid,
    conversation_id: Option<Uuid>,
    session_at: DateTime<Utc>,
}

#[async_trait]
impl EntityStore for PostgresMemory {
    async fn entity_work(&self, job: &ExtractEntities) -> Result<EntityWork, EntityStoreError> {
        let row = leaf_row(&self.pool, job.summary_id).await?;
        if let Some(wanted) = job.conversation_id
            && row.conversation_id.is_some_and(|id| id != wanted)
        {
            return Err(EntityStoreError::Query(Box::new(std::io::Error::other(
                "conversation does not own summary",
            ))));
        }
        if let Some(entities) = done_entities(&self.pool, job.summary_id).await? {
            return Ok(EntityWork::Complete { entities });
        }
        Ok(EntityWork::Pending(leaf_from(row)))
    }

    async fn following(
        &self,
        lead: &EntityLeaf,
        limit: u32,
    ) -> Result<Vec<FollowingLeaf>, EntityStoreError> {
        let Some(conversation) = lead.conversation_id else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query_as::<_, SiblingRow>(FOLLOWING)
            .bind(conversation)
            .bind(lead.start)
            .bind(lead.id)
            .bind(ENTITY_PIPELINE_VERSION)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(query_error)?;
        Ok(rows.into_iter().map(sibling_from).collect())
    }

    async fn observe_context(
        &self,
        leaves: &[EntityLeaf],
    ) -> Result<ObserveContext, EntityStoreError> {
        let types = type_paths(&self.pool).await?;
        let known = known_entities(&self.pool, leaves).await?;
        Ok(ObserveContext { types, known })
    }

    async fn transcript(
        &self,
        leaves: &[EntityLeaf],
    ) -> Result<Vec<TranscriptMessage>, EntityStoreError> {
        load_transcript(&self.pool, leaves).await
    }

    async fn commit(
        &self,
        leaves: &[EntityLeaf],
        prepared: &PreparedEntity,
        now: DateTime<Utc>,
    ) -> Result<Option<EntityView>, EntityStoreError> {
        let Some(owner) = leaves.iter().find_map(|leaf| leaf.owner.clone()) else {
            return Ok(None);
        };
        let started = leaves.first().map_or(now, |leaf| leaf.start);
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        lock_names(&mut tx, prepared).await?;
        let mut types = type_rows(&mut tx).await?;
        let Some(kind) = ensure_type(&mut tx, &prepared.type_name, &mut types, &owner, now).await?
        else {
            tracing::info!(
                "extract-entities: skipped {} with no type name",
                prepared.name
            );
            return Ok(None);
        };
        let summary = leaves.first().map(|leaf| leaf.id);
        let resolved = resolve(
            &mut tx,
            prepared,
            ResolveAt {
                kind: &kind,
                types: &types,
                owner: &owner,
                summary,
                started,
                now,
            },
        )
        .await?;
        write_aliases(&mut tx, resolved.id, prepared).await?;
        super::claims::assert_snapshot(&mut tx, resolved.id, &prepared.facts, "", &owner).await?;
        write_history(&mut tx, resolved.id, prepared, &owner).await?;
        let view = load_view(&mut tx, resolved.id).await?;
        tx.commit().await.map_err(query_error)?;
        Ok(Some(view))
    }

    async fn publish(
        &self,
        view: &EntityView,
        prose: &EntityProse,
        now: DateTime<Utc>,
    ) -> Result<PublishOutcome, EntityStoreError> {
        let directory = self.wiki_home("").map_err(store_error)?;
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        lock_view(&mut tx, view).await?;
        let fresh = load_view(&mut tx, view.id).await?;
        if fresh.snapshot != view.snapshot {
            return Ok(PublishOutcome::Stale(Box::new(fresh)));
        }
        commit_page(tx, &directory, &fresh, prose, now).await
    }

    async fn lease_entity(&self, id: Uuid) -> Result<Box<dyn EntityLease>, EntityStoreError> {
        let directory = self.wiki_home("").map_err(store_error)?;
        let options = self.pool.connect_options();
        let mut conn = sqlx::PgConnection::connect_with(options.as_ref())
            .await
            .map_err(query_error)?;
        crate::scope::apply(&mut conn).await.map_err(query_error)?;
        ignore_idle_timeout(&mut conn).await;
        let key = format!("id:{id}");
        sqlx::query("SELECT pg_advisory_lock($1, hashtext($2))")
            .bind(ENTITY_LOCK_CLASS)
            .bind(&key)
            .execute(&mut conn)
            .await
            .map_err(query_error)?;
        Ok(Box::new(PgLease {
            conn: Some(conn),
            id,
            directory,
        }))
    }

    async fn finish_entities(
        &self,
        leaves: &[Uuid],
        slugs: &[String],
        git_sha: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        for id in leaves {
            sqlx::query(FINISH)
                .bind(id)
                .bind(ENTITY_PIPELINE_VERSION)
                .bind(slugs)
                .bind(git_sha)
                .bind(now)
                .execute(&self.pool)
                .await
                .map_err(query_error)?;
        }
        Ok(())
    }

    async fn fail_entity(
        &self,
        summary_id: Uuid,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        sqlx::query(FAIL)
            .bind(summary_id)
            .bind(ENTITY_PIPELINE_VERSION)
            .bind(error)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(query_error)?;
        Ok(())
    }

    async fn entity_backfill(
        &self,
        budget: BackfillBudget,
    ) -> Result<Vec<ExtractEntities>, EntityStoreError> {
        let rows = sqlx::query_as::<_, JobRow>(BACKFILL)
            .bind(ENTITY_PIPELINE_VERSION)
            .bind(i64::from(budget.idle))
            .bind(i64::from(budget.busy))
            .bind(BACKFILL_PRIORITY)
            .fetch_all(&self.pool)
            .await
            .map_err(query_error)?;
        Ok(rows.iter().map(job_from).collect())
    }

    async fn stale_entities(&self, limit: u32) -> Result<Vec<ExtractEntities>, EntityStoreError> {
        let rows = sqlx::query_as::<_, JobRow>(STALE)
            .bind(ENTITY_PIPELINE_VERSION)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(query_error)?;
        Ok(rows.iter().map(job_from).collect())
    }

    async fn revive_dead_entities(&self, limit: u32) -> Result<u64, EntityStoreError> {
        GraphileQueue::new(self.pool.clone())
            .reschedule_dead(
                &JobName::Fixed(FixedJob::ExtractEntities),
                limit,
                BACKFILL_PRIORITY,
            )
            .await
            .map_err(|error| EntityStoreError::Query(Box::new(error)))
    }

    async fn match_candidates(
        &self,
        query: &CandidateQuery,
    ) -> Result<Vec<MatchCandidate>, EntityStoreError> {
        gather(&self.pool, query).await
    }

    async fn type_briefs(&self) -> Result<Vec<TypeBrief>, EntityStoreError> {
        super::types::type_briefs(&self.pool).await
    }

    async fn remember_type_alias(
        &self,
        type_name: &str,
        alias: &str,
    ) -> Result<(), EntityStoreError> {
        super::types::remember_type_alias(&self.pool, type_name, alias).await
    }

    async fn type_names(&self) -> Result<Vec<String>, EntityStoreError> {
        type_paths(&self.pool).await
    }

    async fn record_decision(
        &self,
        summary: Uuid,
        decision: &MatchDecision,
        now: DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        let payload = serde_json::to_string(&decision.candidates)
            .map_err(|error| EntityStoreError::Query(Box::new(error)))?;
        let choice = (!decision.choice.is_empty()).then(|| decision.choice.clone());
        let inserted = sqlx::query(RECORD_DECISION)
            .bind(Uuid::new_v4())
            .bind(summary)
            .bind(&decision.kind)
            .bind(&decision.proposed_name)
            .bind(&decision.proposed_type)
            .bind(payload)
            .bind(&decision.verdict)
            .bind(choice)
            .bind(decision.confidence)
            .bind(&decision.model)
            .bind(decision.entity_id)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(query_error)?;
        if inserted.rows_affected() == 0 {
            return Err(EntityStoreError::Missing(summary));
        }
        Ok(())
    }

    async fn remember_embedding(
        &self,
        entity: Uuid,
        embedding: &[f32],
        now: DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        if embedding.len() != 1024 {
            return Ok(());
        }
        sqlx::query(REMEMBER)
            .bind(entity)
            .bind(HalfVector::from_f32_slice(embedding))
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(query_error)?;
        Ok(())
    }

    async fn hold_candidate(
        &self,
        summary: Uuid,
        prepared: &PreparedEntity,
        confidence: f32,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        let inserted = sqlx::query(HOLD)
            .bind(Uuid::new_v4())
            .bind(summary)
            .bind(&prepared.name)
            .bind(&prepared.type_name)
            .bind(confidence)
            .bind(reason)
            .bind(hold_payload(prepared))
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(query_error)?;
        if inserted.rows_affected() == 0 {
            return Err(EntityStoreError::Missing(summary));
        }
        Ok(())
    }

    async fn merge_entities(
        &self,
        left: Uuid,
        right: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<EntityMerge>, EntityStoreError> {
        if left == right {
            return Ok(None);
        }
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        let (merged, removed) = super::merge::merge_pair(&mut tx, left, right, now)
            .await
            .map_err(query_error)?;
        tx.commit().await.map_err(query_error)?;
        if let Some(page) = removed
            && let Err(error) = super::page_file::remove_page_file(self, &page).await
        {
            tracing::warn!(%error, page = %page.local, "merged entity page file was not removed");
        }
        Ok(merged)
    }
}

fn leaf_from(row: LeafRow) -> EntityLeaf {
    let end = if row.end_at < row.start_at {
        row.start_at
    } else {
        row.end_at
    };
    EntityLeaf {
        id: row.id,
        conversation_id: row.conversation_id,
        content: row.content,
        start: row.start_at,
        end,
        owner: Some(row.owner_user_id),
    }
}

fn sibling_from(row: SiblingRow) -> FollowingLeaf {
    FollowingLeaf {
        job: row.job_id.map(JobId::new),
        done: row.done,
        leaf: leaf_from(LeafRow {
            id: row.id,
            conversation_id: row.conversation_id,
            content: row.content,
            start_at: row.start_at,
            end_at: row.end_at,
            owner_user_id: row.owner_user_id,
        }),
    }
}

fn job_from(row: &JobRow) -> ExtractEntities {
    ExtractEntities {
        summary_id: row.id,
        conversation_id: row.conversation_id,
        session_at: Some(row.session_at),
    }
}

async fn leaf_row(pool: &sqlx::PgPool, id: Uuid) -> Result<LeafRow, EntityStoreError> {
    sqlx::query_as::<_, LeafRow>(LEAF)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(query_error)?
        .ok_or(EntityStoreError::Missing(id))
}

async fn done_entities(
    pool: &sqlx::PgPool,
    id: Uuid,
) -> Result<Option<Vec<String>>, EntityStoreError> {
    let row = sqlx::query_as::<_, DoneRow>(DONE)
        .bind(id)
        .bind(ENTITY_PIPELINE_VERSION)
        .fetch_optional(pool)
        .await
        .map_err(query_error)?;
    Ok(row.map(|row| row.entities_touched))
}

async fn lock_names(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedEntity,
) -> Result<(), EntityStoreError> {
    let mut keys: Vec<String> = name_keys(prepared)
        .into_iter()
        .map(|key| format!("name:{key}"))
        .collect();
    if let Some(id) = prepared.entity_ref {
        keys.push(format!("id:{id}"));
    }
    if keys.is_empty() {
        keys.push(format!("name:{}", compact_key(&prepared.name)));
    }
    lock_all(tx, &keys).await
}

fn name_keys(prepared: &PreparedEntity) -> Vec<String> {
    identity_keys(&prepared.name)
}

async fn lock_view(
    tx: &mut Transaction<'_, Postgres>,
    view: &EntityView,
) -> Result<(), EntityStoreError> {
    lock_all(
        tx,
        &[
            format!("name:{}", compact_key(&view.name)),
            format!("id:{}", view.id),
        ],
    )
    .await
}

async fn lock_all(
    tx: &mut Transaction<'_, Postgres>,
    keys: &[String],
) -> Result<(), EntityStoreError> {
    let mut ordered = keys.to_vec();
    ordered.sort();
    ordered.dedup();
    for key in ordered {
        sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
            .bind(ENTITY_LOCK_CLASS)
            .bind(key)
            .execute(&mut **tx)
            .await
            .map_err(query_error)?;
    }
    Ok(())
}
