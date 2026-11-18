mod fence;

use async_trait::async_trait;
use fence::{lock_names, lock_view, validate_leaf};
use jiff::Timestamp;
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

use super::error::fence_error;
use crate::PostgresMemory;
use crate::graphile::GraphileQueue;
use crate::summary_replace;
use kernel::memory::summary_reference::{SummaryConsumer, SummaryReferenceError};

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
    input_key: Option<Vec<u8>>,
    id: Uuid,
    conversation_id: Option<Uuid>,
    content: String,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    start_at: Timestamp,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    end_at: Timestamp,
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
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    start_at: Timestamp,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    end_at: Timestamp,
    owner_user_id: String,
    done: bool,
    job_id: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
struct JobRow {
    id: Uuid,
    conversation_id: Option<Uuid>,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    session_at: Timestamp,
}

#[async_trait]
impl EntityStore for PostgresMemory {
    async fn entity_work(&self, job: &ExtractEntities) -> Result<EntityWork, EntityStoreError> {
        let row = leaf_row(&self.pool, job.summary_id).await?;
        if let Some(wanted) = job.conversation_id
            && row.conversation_id != Some(wanted)
        {
            return Err(SummaryReferenceError::Stale(job.summary_id).into());
        }
        if let Some(key) = row.input_key.clone() {
            let mut conn = self.pool.acquire().await.map_err(query_error)?;
            let pending =
                summary_replace::pending(&mut conn, job.summary_id, SummaryConsumer::Entity)
                    .await
                    .map_err(query_error)?;
            return if pending.is_empty() {
                Ok(EntityWork::Complete {
                    entities: Vec::new(),
                })
            } else {
                let mut leaf = leaf_from(row);
                leaf.delivery = Some(kernel::wiki::builder::SummaryDelivery {
                    input_key: key,
                    pending,
                });
                Ok(EntityWork::Pending(leaf))
            };
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
        if lead.delivery.is_some() {
            return Ok(Vec::new());
        }
        let Some(conversation) = lead.conversation_id else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query_as::<_, SiblingRow>(FOLLOWING)
            .bind(conversation)
            .bind(crate::time::BindTime::bind_time(lead.start))
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
        now: Timestamp,
    ) -> Result<Option<EntityView>, EntityStoreError> {
        let Some(owner) = leaves.iter().find_map(|leaf| leaf.owner.clone()) else {
            return Ok(None);
        };
        let started = leaves.first().map_or(now, |leaf| leaf.start);
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        let ids: Vec<_> = leaves
            .iter()
            .map(|leaf| leaf.id)
            .chain(prepared.facts.iter().map(|fact| fact.summary_id))
            .chain(prepared.history.iter().map(|item| item.summary_id))
            .collect();
        let inputs = summary_replace::shared(&mut tx, &ids)
            .await
            .map_err(fence_error)?;
        for input in &inputs {
            if input.owner_user_id != owner {
                return Err(SummaryReferenceError::Stale(input.id).into());
            }
            if let Some(leaf) = leaves.iter().find(|leaf| leaf.id == input.id) {
                validate_leaf(&mut tx, input, leaf).await?;
            }
        }
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
        now: Timestamp,
    ) -> Result<PublishOutcome, EntityStoreError> {
        let directory = self.wiki_home("").map_err(store_error)?;
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        let ids: Vec<_> = view.history.iter().map(|line| line.summary_id).collect();
        summary_replace::shared(&mut tx, &ids)
            .await
            .map_err(fence_error)?;
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
        now: Timestamp,
    ) -> Result<(), EntityStoreError> {
        finish(self, leaves, &[], slugs, git_sha, now).await
    }

    async fn finish_entity_delivery(
        &self,
        leaves: &[EntityLeaf],
        slugs: &[String],
        git_sha: Option<&str>,
        now: Timestamp,
    ) -> Result<(), EntityStoreError> {
        let ids: Vec<_> = leaves.iter().map(|leaf| leaf.id).collect();
        finish(self, &ids, leaves, slugs, git_sha, now).await
    }

    async fn fail_entity(
        &self,
        summary_id: Uuid,
        error: &str,
        now: Timestamp,
    ) -> Result<(), EntityStoreError> {
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        summary_replace::shared(&mut tx, &[summary_id])
            .await
            .map_err(fence_error)?;
        sqlx::query(FAIL)
            .bind(summary_id)
            .bind(ENTITY_PIPELINE_VERSION)
            .bind(error)
            .bind(crate::time::BindTime::bind_time(now))
            .execute(&mut *tx)
            .await
            .map_err(query_error)?;
        tx.commit().await.map_err(query_error)
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

    async fn record_decision(
        &self,
        summary: Uuid,
        decision: &MatchDecision,
        now: Timestamp,
    ) -> Result<(), EntityStoreError> {
        let payload = serde_json::to_string(&decision.candidates)
            .map_err(|error| EntityStoreError::Query(Box::new(error)))?;
        let choice = (!decision.choice.is_empty()).then(|| decision.choice.clone());
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        summary_replace::shared(&mut tx, &[summary])
            .await
            .map_err(fence_error)?;
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
            .bind(crate::time::BindTime::bind_time(now))
            .execute(&mut *tx)
            .await
            .map_err(query_error)?;
        if inserted.rows_affected() == 0 {
            return Err(EntityStoreError::Missing(summary));
        }
        tx.commit().await.map_err(query_error)
    }

    async fn remember_embedding(
        &self,
        entity: Uuid,
        embedding: &[f32],
        now: Timestamp,
    ) -> Result<(), EntityStoreError> {
        if embedding.len() != 1024 {
            return Ok(());
        }
        sqlx::query(REMEMBER)
            .bind(entity)
            .bind(HalfVector::from_f32_slice(embedding))
            .bind(crate::time::BindTime::bind_time(now))
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
        now: Timestamp,
    ) -> Result<(), EntityStoreError> {
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        summary_replace::shared(&mut tx, &[summary])
            .await
            .map_err(fence_error)?;
        let inserted = sqlx::query(HOLD)
            .bind(Uuid::new_v4())
            .bind(summary)
            .bind(&prepared.name)
            .bind(&prepared.type_name)
            .bind(confidence)
            .bind(reason)
            .bind(hold_payload(prepared))
            .bind(crate::time::BindTime::bind_time(now))
            .execute(&mut *tx)
            .await
            .map_err(query_error)?;
        if inserted.rows_affected() == 0 {
            return Err(EntityStoreError::Missing(summary));
        }
        tx.commit().await.map_err(query_error)
    }

    async fn merge_entities(
        &self,
        left: Uuid,
        right: Uuid,
        now: Timestamp,
    ) -> Result<Option<EntityMerge>, EntityStoreError> {
        if left == right {
            return Ok(None);
        }
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT summary_id FROM rh_entity_history WHERE entity_id = ANY($1)
             UNION SELECT summary_id FROM rh_entity_applied WHERE entity_id = ANY($1)
             UNION SELECT summary_id FROM rh_entity_type_seen WHERE entity_id = ANY($1)
             UNION SELECT c.summary_id FROM rh_entity_fact_claims c
             JOIN rh_entity_facts f ON f.id = c.fact_id WHERE f.topic_id = ANY($1) AND c.summary_id IS NOT NULL
             UNION SELECT c.summary_id FROM rh_wiki_citations c
             JOIN rh_wiki_topics p ON p.slug = c.topic_slug WHERE p.topic_id = ANY($1)
             UNION SELECT p.source_id FROM rh_wiki_provenance p
             JOIN rh_wiki_topics w ON w.slug = p.topic_slug WHERE w.topic_id = ANY($1) AND p.source_kind = 'summary'",
        ).bind([left, right].as_slice()).fetch_all(&mut *tx).await.map_err(query_error)?;
        summary_replace::shared_visible(&mut tx, &ids)
            .await
            .map_err(fence_error)?;
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
        delivery: None,
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
            input_key: None,
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

async fn finish(
    store: &PostgresMemory,
    ids: &[Uuid],
    leaves: &[EntityLeaf],
    slugs: &[String],
    git_sha: Option<&str>,
    now: Timestamp,
) -> Result<(), EntityStoreError> {
    let mut tx = store.pool.begin().await.map_err(query_error)?;
    let inputs = summary_replace::shared(&mut tx, ids)
        .await
        .map_err(fence_error)?;
    for input in &inputs {
        let leaf = leaves.iter().find(|leaf| leaf.id == input.id);
        if let Some(leaf) = leaf {
            validate_leaf(&mut tx, input, leaf).await?;
        } else if input.input_key.is_some() {
            return Err(SummaryReferenceError::Stale(input.id).into());
        }
        sqlx::query(FINISH)
            .bind(input.id)
            .bind(ENTITY_PIPELINE_VERSION)
            .bind(slugs)
            .bind(git_sha)
            .bind(crate::time::BindTime::bind_time(now))
            .execute(&mut *tx)
            .await
            .map_err(query_error)?;
        if let Some(delivery) = leaf.and_then(|leaf| leaf.delivery.as_ref()) {
            summary_replace::complete(
                &mut tx,
                input.id,
                SummaryConsumer::Entity,
                &delivery.pending,
                ENTITY_PIPELINE_VERSION,
            )
            .await
            .map_err(fence_error)?;
        } else {
            summary_replace::complete_legacy(
                &mut tx,
                input.id,
                SummaryConsumer::Entity,
                ENTITY_PIPELINE_VERSION,
            )
            .await
            .map_err(query_error)?;
        }
    }
    tx.commit().await.map_err(query_error)
}
