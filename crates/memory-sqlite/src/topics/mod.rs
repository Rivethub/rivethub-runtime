mod bulk;
mod decisions;
mod filing;
mod history;
mod inherited;
mod mentions;
mod rows;
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
use rusqlite::Connection;
use rusqlite::params;
use wire::session_id::SessionId;

use crate::SqliteStore;
use crate::error::SqliteError;
use rows::failed;
use rows::not_found;

impl SqliteStore {
    async fn topics<T, F>(&self, operation: &'static str, work: F) -> Result<T, TopicError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, TopicError> + Send + 'static,
    {
        self.with_conn(move |conn| {
            let tx = conn.transaction().map_err(SqliteError::query(operation))?;
            let done = work(&tx);
            if done.is_ok() {
                tx.commit().map_err(SqliteError::query(operation))?;
            }
            Ok(done)
        })
        .await
        .map_err(|error| TopicError::Unavailable(Box::new(error)))?
    }
}

fn stored(conn: &Connection, id: TopicId) -> Result<Topic, TopicError> {
    rows::by_id(conn, id)?.ok_or_else(|| not_found(id))
}

#[async_trait]
impl TopicStore for SqliteStore {
    async fn create(&self, topic: Topic) -> Result<Topic, TopicError> {
        self.topics("topic create", move |conn| {
            if let Some(parent) = topic.parent {
                let parent = rows::by_id(conn, parent)?.ok_or_else(|| not_found(parent))?;
                check_parent(&topic, &parent, &rows::tree(conn)?)?;
            }
            write::claim_names(conn, &topic)?;
            write::insert(conn, &topic)?;
            write::aliases(conn, &topic)?;
            write::enqueue_syncs(conn, SyncTopicPage::after_create(&topic))?;
            stored(conn, topic.id)
        })
        .await
        .inspect(|_| self.notifications.send_replace(()))
    }

    async fn update(
        &self,
        id: TopicId,
        changes: TopicChanges,
        now: DateTime<Utc>,
    ) -> Result<Topic, TopicError> {
        self.topics("topic update", move |conn| {
            let current = rows::by_id(conn, id)?.ok_or_else(|| not_found(id))?;
            let before = current.clone();
            let updated = current.apply(changes, now)?;
            if let Some(parent) = updated.parent {
                let parent = rows::by_id(conn, parent)?.ok_or_else(|| not_found(parent))?;
                check_parent(&updated, &parent, &rows::tree(conn)?)?;
            }
            write::claim_names(conn, &updated)?;
            write::row(conn, &updated)?;
            write::aliases(conn, &updated)?;
            if updated.slug != before.slug {
                write::rename_redirect(conn, &before.slug, &updated)?;
            }
            write::enqueue_syncs(conn, SyncTopicPage::after_update(&before, &updated))?;
            stored(conn, id)
        })
        .await
        .inspect(|_| self.notifications.send_replace(()))
    }

    async fn merge(
        &self,
        source: TopicId,
        into: TopicId,
        now: DateTime<Utc>,
    ) -> Result<Topic, TopicError> {
        self.topics("topic merge", move |conn| {
            let from = rows::by_id(conn, source)?.ok_or_else(|| not_found(source))?;
            let target = rows::by_id(conn, into)?.ok_or_else(|| not_found(into))?;
            let plan = MergePlan::new(&from, &target, &rows::tree(conn)?)?;
            let syncs = SyncTopicPage::after_merge(&from, &target, write::page_of(conn, source)?);
            let merged = plan.merged(target, now);
            write::fold_into(conn, &plan, &merged)?;
            write::enqueue_syncs(conn, syncs)?;
            stored(conn, into)
        })
        .await
        .inspect(|_| self.notifications.send_replace(()))
    }

    async fn delete(&self, id: TopicId) -> Result<(), TopicError> {
        self.topics("topic delete", move |conn| {
            let topic = rows::by_id(conn, id)?.ok_or_else(|| not_found(id))?;
            topic.check_delete(!rows::subtopics(conn, id)?.is_empty())?;
            write::remove(conn, &topic)?;
            write::enqueue_syncs(conn, SyncTopicPage::after_delete(&topic))
        })
        .await
        .inspect(|()| self.notifications.send_replace(()))
    }

    async fn set_closed(
        &self,
        id: TopicId,
        closed: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<Topic, TopicError> {
        self.topics("topic close", move |conn| {
            let topic = rows::by_id(conn, id)?
                .ok_or_else(|| not_found(id))?
                .closed(closed, now);
            write::closure(conn, &topic)?;
            stored(conn, id)
        })
        .await
    }

    async fn find(&self, key: &TopicKey) -> Result<Option<Topic>, TopicError> {
        let key = key.clone();
        self.topics("topic read", move |conn| match &key {
            TopicKey::Id(id) => rows::by_id(conn, *id),
            TopicKey::Slug(kind, slug) => rows::by_slug(conn, *kind, slug),
        })
        .await
    }

    async fn list(&self) -> Result<Vec<TopicListing>, TopicError> {
        let mut listings = self.topics("topic list", rows::listings).await?;
        listings.sort_by(by_activity);
        Ok(listings)
    }

    async fn redirects(&self) -> Result<Vec<Redirect>, TopicError> {
        self.topics("topic redirects", rows::redirects).await
    }

    async fn place(&self, placement: Placement) -> Result<PlacementOutcome, TopicError> {
        self.topics("session placement", move |conn| {
            rows::by_id(conn, placement.topic)?
                .ok_or_else(|| not_found(placement.topic))?
                .accepts_sessions()?;
            let existing = rows::placement_of(conn, &placement.session)?;
            match placement.admit(existing) {
                PlacementOutcome::Placed(placed) => {
                    write::placement(conn, &placed)?;
                    rows::placement_of(conn, &placed.session)?
                        .map(PlacementOutcome::Placed)
                        .ok_or_else(|| not_found(placed.topic))
                }
                kept @ PlacementOutcome::Kept(_) => Ok(kept),
            }
        })
        .await
    }

    async fn unplace(&self, session: &SessionId) -> Result<Option<Placement>, TopicError> {
        let session = session.clone();
        self.topics("session unplace", move |conn| {
            let existing = rows::placement_of(conn, &session)?;
            conn.execute(
                "DELETE FROM rh_session_placements WHERE session_id = ?1",
                params![session.as_str()],
            )
            .map_err(failed("session unplace"))?;
            Ok(existing)
        })
        .await
    }

    async fn placement(&self, session: &SessionId) -> Result<Option<Placement>, TopicError> {
        let session = session.clone();
        self.topics("session placement read", move |conn| {
            rows::placement_of(conn, &session)
        })
        .await
    }

    async fn sessions(&self, topic: TopicId) -> Result<Vec<PlacedSession>, TopicError> {
        self.topics("topic sessions", move |conn| rows::placed_in(conn, topic))
            .await
    }

    async fn mark_history(&self, mark: HistoryMark) -> Result<HistoryMark, TopicError> {
        self.topics("session history mark", move |conn| {
            history::keep(conn, &mark)
        })
        .await
    }

    async fn unmark_history(&self, session: &SessionId) -> Result<(), TopicError> {
        let session = session.clone();
        self.topics("session history unmark", move |conn| {
            history::forget(conn, &session)
        })
        .await
    }

    async fn history(&self) -> Result<Vec<HistoryMark>, TopicError> {
        self.topics("session history", history::marks).await
    }

    async fn suggest(&self, suggestion: Suggestion) -> Result<(), TopicError> {
        self.topics("session suggestion", move |conn| {
            if rows::placement_of(conn, &suggestion.session)?.is_none() {
                write::suggestion(conn, &suggestion)?;
            }
            Ok(())
        })
        .await
    }

    async fn suggestion(&self, session: &SessionId) -> Result<Option<Suggestion>, TopicError> {
        let session = session.clone();
        self.topics("session suggestion read", move |conn| {
            rows::suggestion_of(conn, &session)
        })
        .await
    }

    async fn suggestions(&self) -> Result<Vec<Suggestion>, TopicError> {
        self.topics("session suggestions", bulk::suggestions).await
    }

    async fn page_slug(&self, topic: TopicId) -> Result<Option<WikiSlug>, TopicError> {
        self.topics("topic page", move |conn| write::page_of(conn, topic))
            .await
    }

    async fn tombstoned(&self, kind: TopicKind, slug: &Slug) -> Result<bool, TopicError> {
        let slug = slug.clone();
        self.topics("topic tombstone read", move |conn| {
            conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM rh_topic_tombstones WHERE kind = ?1 AND slug = ?2)",
                params![kind.to_string(), slug.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(failed("topic tombstone read"))
        })
        .await
    }
}
