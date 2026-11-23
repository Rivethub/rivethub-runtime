mod candidates;
mod claims;
mod filed;
mod identity;
mod known;
mod lease;
mod locks;
mod merge;
mod pages;
mod transcript;
mod types;
mod work;

use async_trait::async_trait;
use jiff::Timestamp;
use kernel::wiki::builder::BackfillBudget;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::entity_pass::{
    CandidateQuery, EntityLeaf, EntityLease, EntityMerge, EntityProse, EntityStore,
    EntityStoreError, EntityView, EntityWork, ExtractEntities, FollowingLeaf, MatchCandidate,
    MatchDecision, ObserveContext, PreparedEntity, PublishOutcome, TranscriptMessage, TypeBrief,
};
use rusqlite::{Connection, Params, Row, Transaction};
use uuid::Uuid;

use crate::SqliteStore;
use crate::summary_replace::{self, FenceError};

#[async_trait]
impl EntityStore for SqliteStore {
    async fn entity_work(&self, job: &ExtractEntities) -> Result<EntityWork, EntityStoreError> {
        let job = job.clone();
        read(self, move |conn| work::load(conn, &job)).await
    }

    async fn following(
        &self,
        lead: &EntityLeaf,
        limit: u32,
    ) -> Result<Vec<FollowingLeaf>, EntityStoreError> {
        let lead = lead.clone();
        let rows = read(self, move |conn| work::following(conn, &lead, limit)).await?;
        rows.into_iter()
            .map(|(leaf, done, job)| {
                let job = job
                    .as_deref()
                    .map(|id| self.remember_claim(id))
                    .transpose()
                    .map_err(error)?;
                Ok(FollowingLeaf { leaf, done, job })
            })
            .collect()
    }

    async fn observe_context(
        &self,
        leaves: &[EntityLeaf],
    ) -> Result<ObserveContext, EntityStoreError> {
        let leaves = leaves.to_vec();
        read(self, move |conn| {
            Ok(ObserveContext {
                types: types::paths(conn)?,
                known: known::find(conn, &leaves)?,
            })
        })
        .await
    }

    async fn transcript(
        &self,
        leaves: &[EntityLeaf],
    ) -> Result<Vec<TranscriptMessage>, EntityStoreError> {
        let leaves = leaves.to_vec();
        read(self, move |conn| transcript::load(conn, &leaves)).await
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
        let id = prepared.entity_ref;
        let leaves = leaves.to_vec();
        let prepared = prepared.clone();
        locks::scoped(self, id, move |tx| {
            identity::commit(tx, &leaves, &prepared, &owner, now)
        })
        .await
    }

    async fn publish(
        &self,
        view: &EntityView,
        prose: &EntityProse,
        now: Timestamp,
    ) -> Result<PublishOutcome, EntityStoreError> {
        let view = view.clone();
        let prose = prose.clone();
        let Some(directory) = self.wiki_directory().map(std::path::Path::to_path_buf) else {
            return locks::scoped(self, [view.id], move |tx| {
                pages::publish(tx, &view, &prose, now)
            })
            .await;
        };
        filed::publish(self, &directory, &view, &prose, now).await
    }

    async fn finish_entities(
        &self,
        leaves: &[Uuid],
        slugs: &[String],
        git_sha: Option<&str>,
        now: Timestamp,
    ) -> Result<(), EntityStoreError> {
        let leaves = leaves.to_vec();
        let slugs = slugs.to_vec();
        let sha = git_sha.map(str::to_owned);
        write(self, move |tx| {
            work::finish(tx, &leaves, &[], &slugs, sha.as_deref(), now)
        })
        .await
    }

    async fn finish_entity_delivery(
        &self,
        leaves: &[EntityLeaf],
        slugs: &[String],
        git_sha: Option<&str>,
        now: Timestamp,
    ) -> Result<(), EntityStoreError> {
        let leaves = leaves.to_vec();
        let slugs = slugs.to_vec();
        let sha = git_sha.map(str::to_owned);
        write(self, move |tx| {
            let ids: Vec<_> = leaves.iter().map(|leaf| leaf.id).collect();
            work::finish(tx, &ids, &leaves, &slugs, sha.as_deref(), now)
        })
        .await
    }

    async fn fail_entity(
        &self,
        summary_id: Uuid,
        error: &str,
        now: Timestamp,
    ) -> Result<(), EntityStoreError> {
        let error = error.to_owned();
        write(self, move |tx| work::fail(tx, summary_id, &error, now)).await
    }

    async fn entity_backfill(
        &self,
        budget: BackfillBudget,
    ) -> Result<Vec<ExtractEntities>, EntityStoreError> {
        let now = self.clock.now();
        read(self, move |conn| work::backfill(conn, budget, now)).await
    }

    async fn stale_entities(&self, limit: u32) -> Result<Vec<ExtractEntities>, EntityStoreError> {
        let now = self.clock.now();
        read(self, move |conn| work::stale(conn, limit, now)).await
    }

    async fn revive_dead_entities(&self, limit: u32) -> Result<u64, EntityStoreError> {
        let now = self.clock.now();
        let revived = write(self, move |tx| work::revive(tx, limit, now)).await?;
        if revived > 0 {
            self.notifications.send_replace(());
        }
        Ok(revived)
    }

    async fn match_candidates(
        &self,
        query: &CandidateQuery,
    ) -> Result<Vec<MatchCandidate>, EntityStoreError> {
        let query = query.clone();
        read(self, move |conn| candidates::gather(conn, &query)).await
    }

    async fn remember_embedding(
        &self,
        entity: Uuid,
        embedding: &[f32],
        now: Timestamp,
    ) -> Result<(), EntityStoreError> {
        let embedding = embedding.to_vec();
        write(self, move |tx| {
            candidates::remember(tx, entity, &embedding, now)
        })
        .await
    }

    async fn hold_candidate(
        &self,
        summary: Uuid,
        prepared: &PreparedEntity,
        confidence: f32,
        reason: &str,
        now: Timestamp,
    ) -> Result<(), EntityStoreError> {
        let prepared = prepared.clone();
        let reason = reason.to_owned();
        write(self, move |tx| {
            work::hold(tx, summary, &prepared, confidence, &reason, now)
        })
        .await
    }

    async fn record_decision(
        &self,
        summary: Uuid,
        decision: &MatchDecision,
        now: Timestamp,
    ) -> Result<(), EntityStoreError> {
        let decision = decision.clone();
        write(self, move |tx| work::decision(tx, summary, &decision, now)).await
    }

    async fn type_briefs(&self) -> Result<Vec<TypeBrief>, EntityStoreError> {
        read(self, types::briefs).await
    }

    async fn remember_type_alias(&self, name: &str, alias: &str) -> Result<(), EntityStoreError> {
        let name = name.to_owned();
        let alias = alias.to_owned();
        write(self, move |tx| types::remember_alias(tx, &name, &alias)).await
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
        let work = locks::scoped(self, [left, right], move |tx| {
            merge::pair(tx, left, right, now)
        })
        .await?;
        if let Some(root) = self.wiki_directory().map(std::path::Path::to_path_buf) {
            if let Some(slug) = work.drop_slug.as_deref()
                && let Err(error) = remove_merged_file(&root, slug).await
            {
                tracing::warn!(%error, slug, "merged entity page file was not removed");
            }
            if let Err(error) = sweep_leftovers(self, &root).await {
                tracing::warn!(%error, "merged entity leftover page was not removed");
            }
        }
        Ok(work.outcome)
    }

    async fn lease_entity(&self, id: Uuid) -> Result<Box<dyn EntityLease>, EntityStoreError> {
        let guard = locks::Guard::acquire(self, id).await?;
        Ok(Box::new(lease::Lease::new(self.clone(), guard)))
    }
}

async fn read<T, F>(store: &SqliteStore, body: F) -> Result<T, EntityStoreError>
where
    T: Send + 'static,
    F: FnOnce(&Connection) -> Result<T, EntityStoreError> + Send + 'static,
{
    store
        .with_conn(move |conn| {
            Ok((|| {
                let tx = conn.transaction().map_err(query)?;
                let result = body(&tx)?;
                tx.commit().map_err(query)?;
                Ok(result)
            })())
        })
        .await
        .map_err(error)?
}

async fn write<T, F>(store: &SqliteStore, body: F) -> Result<T, EntityStoreError>
where
    T: Send + 'static,
    F: FnOnce(&Transaction<'_>) -> Result<T, EntityStoreError> + Send + 'static,
{
    store
        .with_conn(move |conn| {
            Ok((|| {
                let tx = summary_replace::immediate(conn).map_err(query)?;
                let result = body(&tx)?;
                tx.commit().map_err(query)?;
                Ok(result)
            })())
        })
        .await
        .map_err(error)?
}

fn rows<T>(
    conn: &Connection,
    sql: &str,
    params: impl Params,
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, EntityStoreError> {
    conn.prepare(sql)
        .map_err(query)?
        .query_map(params, map)
        .map_err(query)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(query)
}

fn error(error: impl std::error::Error + Send + Sync + 'static) -> EntityStoreError {
    EntityStoreError::Query(Box::new(error))
}

async fn sweep_leftovers(
    store: &SqliteStore,
    root: &std::path::Path,
) -> Result<(), EntityStoreError> {
    let slugs = read(store, leftover_slugs).await?;
    for slug in slugs {
        if let Err(error) = remove_merged_file(root, &slug).await {
            tracing::warn!(%error, slug, "merged entity leftover page was not removed");
        }
    }
    Ok(())
}

fn leftover_slugs(conn: &Connection) -> Result<Vec<String>, EntityStoreError> {
    let mut statement = conn
        .prepare(
            "SELECT from_slug FROM ros_wiki_redirects
              WHERE NOT EXISTS (
                  SELECT 1 FROM ros_wiki_topics WHERE slug = ros_wiki_redirects.from_slug
              )",
        )
        .map_err(query)?;
    let rows = statement.query_map([], |row| row.get(0)).map_err(query)?;
    rows.collect::<rusqlite::Result<Vec<String>>>()
        .map_err(query)
}

async fn remove_merged_file(root: &std::path::Path, slug: &str) -> Result<(), EntityStoreError> {
    let owned = slug.to_owned();
    let mut locked = crate::page_files::Files::entity(root)
        .lock()
        .await
        .map_err(store_error)?;
    let message = format!("entity merge: remove {slug}");
    let removed = locked
        .remove(std::slice::from_ref(&owned), &message)
        .await
        .map(|_| ())
        .map_err(WikiStoreError::from);
    crate::page_files::finish(locked, removed)
        .await
        .map_err(store_error)
}

pub(super) fn store_error(error: WikiStoreError) -> EntityStoreError {
    match error {
        WikiStoreError::DirectoryMissing => EntityStoreError::DirectoryMissing,
        WikiStoreError::Page(error) => EntityStoreError::Page(error),
        WikiStoreError::Reference(error) => EntityStoreError::Reference(error),
        WikiStoreError::Missing(id) => EntityStoreError::Missing(id),
        WikiStoreError::Query(error) => EntityStoreError::Query(error),
        WikiStoreError::ConversationMismatch(id) => {
            EntityStoreError::Query(Box::new(std::io::Error::other(id.to_string())))
        }
        WikiStoreError::SlugConflict(slug)
        | WikiStoreError::TopicMissing(slug)
        | WikiStoreError::InvalidPatch(slug) => {
            EntityStoreError::Query(Box::new(std::io::Error::other(slug)))
        }
    }
}

fn query(error: rusqlite::Error) -> EntityStoreError {
    self::error(error)
}

fn fence(error: FenceError) -> EntityStoreError {
    match error {
        FenceError::Reference(error) => error.into(),
        FenceError::Query(error) => query(error),
    }
}

fn uuid(text: &str) -> Result<Uuid, EntityStoreError> {
    Uuid::parse_str(text).map_err(error)
}

fn optional_uuid(text: Option<&str>) -> Result<Option<Uuid>, EntityStoreError> {
    text.map(uuid).transpose()
}

fn stamp(at: Timestamp) -> String {
    format!("{at:.9}")
}

fn at(text: &str) -> Result<Timestamp, EntityStoreError> {
    crate::time::stored(text, "entity timestamp").map_err(error)
}

pub(crate) use candidates::register;

pub(crate) use pages::history_entries;
