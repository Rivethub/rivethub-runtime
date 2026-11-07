mod bulk;
mod decisions;
mod filing;
mod history;
mod inherited;
mod mentions;
mod rows;
mod startup;
mod write;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::topics::HistoryMark;
use kernel::topics::MergePlan;
use kernel::topics::PlacedSession;
use kernel::topics::Placement;
use kernel::topics::PlacementOutcome;
use kernel::topics::Redirect;
use kernel::topics::Slug;
use kernel::topics::Suggestion;
use kernel::topics::SyncTopicPage;
use kernel::topics::Topic;
use kernel::topics::TopicChanges;
use kernel::topics::TopicError;
use kernel::topics::TopicId;
use kernel::topics::TopicKey;
use kernel::topics::TopicKind;
use kernel::topics::TopicListing;
use kernel::topics::TopicStore;
use kernel::topics::by_activity;
use kernel::topics::check_parent;
use kernel::wiki::page::WikiSlug;
use wire::session_id::SessionId;

use crate::PostgresMemory;
use rows::AliasRow;
use rows::PlacedRow;
use rows::PlacementRow;
use rows::SuggestionRow;
use rows::failed;
use rows::not_found;

#[async_trait]
impl TopicStore for PostgresMemory {
    async fn create(&self, topic: Topic) -> Result<Topic, TopicError> {
        let mut tx = self.pool.begin().await.map_err(failed("topic create"))?;
        if let Some(parent) = topic.parent {
            let parent = rows::visible(&mut tx, parent)
                .await?
                .ok_or_else(|| not_found(parent))?;
            check_parent(&topic, &parent, &write::tree(&mut tx).await?)?;
        }
        write::claim_names(&mut tx, &topic).await?;
        write::insert(&mut tx, &topic).await?;
        write::aliases(&mut tx, &topic).await?;
        write::enqueue_syncs(&mut tx, SyncTopicPage::after_create(&topic)).await?;
        let stored = rows::by_id(&mut tx, topic.id)
            .await?
            .ok_or_else(|| not_found(topic.id))?;
        tx.commit().await.map_err(failed("topic create"))?;
        Ok(stored)
    }

    async fn update(
        &self,
        id: TopicId,
        changes: TopicChanges,
        now: DateTime<Utc>,
    ) -> Result<Topic, TopicError> {
        let mut tx = self.pool.begin().await.map_err(failed("topic update"))?;
        let current = rows::by_id(&mut tx, id)
            .await?
            .ok_or_else(|| not_found(id))?;
        let before = current.clone();
        let updated = current.apply(changes, now)?;
        if let Some(parent) = updated.parent {
            let parent = rows::visible(&mut tx, parent)
                .await?
                .ok_or_else(|| not_found(parent))?;
            check_parent(&updated, &parent, &write::tree(&mut tx).await?)?;
        }
        write::claim_names(&mut tx, &updated).await?;
        write::row(&mut tx, &updated).await?;
        write::aliases(&mut tx, &updated).await?;
        if updated.slug != before.slug {
            write::rename_redirect(&mut tx, &before.slug, &updated).await?;
        }
        write::enqueue_syncs(&mut tx, SyncTopicPage::after_update(&before, &updated)).await?;
        let stored = rows::by_id(&mut tx, id)
            .await?
            .ok_or_else(|| not_found(id))?;
        tx.commit().await.map_err(failed("topic update"))?;
        Ok(stored)
    }

    async fn merge(
        &self,
        source: TopicId,
        into: TopicId,
        now: DateTime<Utc>,
    ) -> Result<Topic, TopicError> {
        let mut tx = self.pool.begin().await.map_err(failed("topic merge"))?;
        let from = rows::by_id(&mut tx, source)
            .await?
            .ok_or_else(|| not_found(source))?;
        let target = rows::by_id(&mut tx, into)
            .await?
            .ok_or_else(|| not_found(into))?;
        let plan = MergePlan::new(&from, &target, &write::tree(&mut tx).await?)?;
        let page = write::page_of(&mut tx, source).await?;
        let syncs = SyncTopicPage::after_merge(&from, &target, page);
        let merged = plan.merged(target, now);
        write::fold_into(&mut tx, &plan, &merged).await?;
        write::enqueue_syncs(&mut tx, syncs).await?;
        let stored = rows::by_id(&mut tx, into)
            .await?
            .ok_or_else(|| not_found(into))?;
        tx.commit().await.map_err(failed("topic merge"))?;
        Ok(stored)
    }

    async fn delete(&self, id: TopicId) -> Result<(), TopicError> {
        let mut tx = self.pool.begin().await.map_err(failed("topic delete"))?;
        let topic = rows::by_id(&mut tx, id)
            .await?
            .ok_or_else(|| not_found(id))?;
        topic.check_delete(!write::subtopics(&mut tx, id).await?.is_empty())?;
        write::remove(&mut tx, &topic).await?;
        write::enqueue_syncs(&mut tx, SyncTopicPage::after_delete(&topic)).await?;
        tx.commit().await.map_err(failed("topic delete"))
    }

    async fn set_closed(
        &self,
        id: TopicId,
        closed: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<Topic, TopicError> {
        let mut tx = self.pool.begin().await.map_err(failed("topic close"))?;
        let topic = rows::by_id(&mut tx, id)
            .await?
            .ok_or_else(|| not_found(id))?
            .closed(closed, now);
        write::closure(&mut tx, &topic).await?;
        let stored = rows::by_id(&mut tx, id)
            .await?
            .ok_or_else(|| not_found(id))?;
        tx.commit().await.map_err(failed("topic close"))?;
        Ok(stored)
    }

    async fn find(&self, key: &TopicKey) -> Result<Option<Topic>, TopicError> {
        let mut conn = self.pool.acquire().await.map_err(failed("topic read"))?;
        match key {
            TopicKey::Id(id) => rows::visible(&mut conn, *id).await,
            TopicKey::Slug(kind, slug) => rows::by_slug(&mut conn, *kind, slug).await,
        }
    }

    async fn list(&self) -> Result<Vec<TopicListing>, TopicError> {
        let topics: Vec<rows::ListingRow> = sqlx::query_as(rows::LISTING)
            .fetch_all(&self.pool)
            .await
            .map_err(failed("topic list"))?;
        let aliases = sqlx::query_as!(
            AliasRow,
            "SELECT topic_id, alias FROM rh_topic_aliases ORDER BY alias_key"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(failed("topic list"))?;
        let mut aliases = rows::grouped(aliases)?;
        let mut listings = topics
            .into_iter()
            .map(|row| {
                let own = aliases
                    .remove(&row.topic.id)
                    .into_iter()
                    .flatten()
                    .collect();
                let topic = rows::topic(row.topic, own)?;
                Ok(TopicListing {
                    topic,
                    session_count: u64::try_from(row.placed_sessions).map_err(|source| {
                        TopicError::Unavailable(Box::new(crate::Error::Integer(source)))
                    })?,
                    last_active: row.last_active,
                })
            })
            .collect::<Result<Vec<_>, TopicError>>()?;
        listings.sort_by(by_activity);
        Ok(listings)
    }

    async fn redirects(&self) -> Result<Vec<Redirect>, TopicError> {
        sqlx::query!("SELECT slug, topic_id FROM rh_topic_redirects ORDER BY slug")
            .fetch_all(&self.pool)
            .await
            .map_err(failed("topic redirects"))?
            .into_iter()
            .map(|row| {
                Ok(Redirect {
                    slug: Slug::fold(&row.slug)?,
                    topic: TopicId::from_uuid(row.topic_id),
                })
            })
            .collect()
    }

    async fn place(&self, placement: Placement) -> Result<PlacementOutcome, TopicError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(failed("session placement"))?;
        write::lock_session(&mut tx, &placement.session).await?;
        rows::by_id(&mut tx, placement.topic)
            .await?
            .ok_or_else(|| not_found(placement.topic))?
            .accepts_sessions()?;
        let existing = rows::placement_of(&mut tx, &placement.session).await?;
        let outcome = match placement.admit(existing) {
            PlacementOutcome::Placed(placed) => {
                write::placement(&mut tx, &placed).await?;
                rows::placement_of(&mut tx, &placed.session)
                    .await?
                    .map(PlacementOutcome::Placed)
                    .ok_or_else(|| not_found(placed.topic))?
            }
            kept @ PlacementOutcome::Kept(_) => kept,
        };
        tx.commit().await.map_err(failed("session placement"))?;
        Ok(outcome)
    }

    async fn unplace(&self, session: &SessionId) -> Result<Option<Placement>, TopicError> {
        sqlx::query_as!(
            PlacementRow,
            "DELETE FROM rh_session_placements WHERE session_id = $1
             RETURNING session_id, topic_id, placed_by, placed_at, owner_user_id",
            session.as_str()
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(failed("session unplace"))?
        .map(rows::placement)
        .transpose()
    }

    async fn placement(&self, session: &SessionId) -> Result<Option<Placement>, TopicError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(failed("session placement read"))?;
        rows::placement_of(&mut conn, session).await
    }

    async fn sessions(&self, topic: TopicId) -> Result<Vec<PlacedSession>, TopicError> {
        sqlx::query_as::<_, PlacedRow>(rows::PLACED)
            .bind(topic.uuid())
            .fetch_all(&self.pool)
            .await
            .map_err(failed("topic sessions"))?
            .into_iter()
            .map(rows::placed)
            .collect()
    }

    async fn mark_history(&self, mark: HistoryMark) -> Result<HistoryMark, TopicError> {
        history::keep(&self.pool, &mark).await
    }

    async fn unmark_history(&self, session: &SessionId) -> Result<(), TopicError> {
        history::forget(&self.pool, session).await
    }

    async fn history(&self) -> Result<Vec<HistoryMark>, TopicError> {
        history::marks(&self.pool).await
    }

    async fn suggest(&self, suggestion: Suggestion) -> Result<(), TopicError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(failed("session suggestion"))?;
        write::lock_session(&mut tx, &suggestion.session).await?;
        if rows::placement_of(&mut tx, &suggestion.session)
            .await?
            .is_none()
        {
            write::suggestion(&mut tx, &suggestion).await?;
        }
        tx.commit().await.map_err(failed("session suggestion"))?;
        Ok(())
    }

    async fn suggestion(&self, session: &SessionId) -> Result<Option<Suggestion>, TopicError> {
        sqlx::query_as!(
            SuggestionRow,
            "SELECT session_id, name, topic_id, parent_id
               FROM rh_session_suggestions WHERE session_id = $1",
            session.as_str()
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(failed("session suggestion read"))?
        .map(rows::suggestion)
        .transpose()
    }

    async fn suggestions(&self) -> Result<Vec<Suggestion>, TopicError> {
        bulk::suggestions(&self.pool).await
    }

    async fn page_slug(&self, topic: TopicId) -> Result<Option<WikiSlug>, TopicError> {
        let mut conn = self.pool.acquire().await.map_err(failed("topic page"))?;
        write::page_of(&mut conn, topic).await
    }

    async fn tombstoned(&self, kind: TopicKind, slug: &Slug) -> Result<bool, TopicError> {
        sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM rh_topic_tombstones WHERE kind = $1 AND slug = $2) AS "gone!""#,
            kind.to_string(),
            slug.as_str()
        )
        .fetch_one(&self.pool)
        .await
        .map_err(failed("topic tombstone read"))
    }
}
