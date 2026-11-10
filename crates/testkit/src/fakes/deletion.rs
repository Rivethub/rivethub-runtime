use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::DeletedSession;
use kernel::memory::MemoryStoreError;
use kernel::memory::SessionActivity;
use kernel::memory::SessionDeletion;
use parking_lot::Mutex;
use wire::session_id::{SessionId, SessionKey};

type Tombstone = (DateTime<Utc>, Option<String>);

#[derive(Debug, Default)]
pub struct FakeSessionDeletion {
    activity: Mutex<BTreeMap<String, SessionActivity>>,
    tombstones: Mutex<BTreeMap<String, Tombstone>>,
    keeps: Mutex<BTreeMap<String, DateTime<Utc>>>,
    trash: Mutex<BTreeMap<String, DateTime<Utc>>>,
}

impl FakeSessionDeletion {
    #[must_use]
    pub fn with_activity(self, session: impl Into<SessionKey>, activity: SessionActivity) -> Self {
        self.activity
            .lock()
            .insert(session.into().as_str().to_owned(), activity);
        self
    }

    pub fn tombstone(&self, session: &SessionId, deleted_at: DateTime<Utc>) {
        self.tombstones
            .lock()
            .insert(session.as_str().to_owned(), (deleted_at, None));
    }

    pub fn keep(&self, session: &SessionId, kept_at: DateTime<Utc>) {
        self.keeps
            .lock()
            .insert(session.as_str().to_owned(), kept_at);
    }

    #[must_use]
    pub fn is_kept(&self, session: impl Into<SessionKey>) -> bool {
        self.keeps.lock().contains_key(session.into().as_str())
    }

    #[must_use]
    pub fn is_trashed(&self, session: impl Into<SessionKey>) -> bool {
        self.trash.lock().contains_key(session.into().as_str())
    }

    #[must_use]
    pub fn activity_of(&self, session: &SessionId) -> Option<SessionActivity> {
        self.activity.lock().get(session.as_str()).cloned()
    }

    #[must_use]
    pub fn deleted_by(&self, session: &SessionId) -> Option<Option<String>> {
        self.tombstones
            .lock()
            .get(session.as_str())
            .map(|(_, deleted_by)| deleted_by.clone())
    }
}

#[async_trait]
impl SessionDeletion for FakeSessionDeletion {
    async fn delete_session(
        &self,
        session_key: &str,
        deleted_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<DeletedSession, MemoryStoreError> {
        self.tombstones
            .lock()
            .insert(session_key.to_owned(), (now, deleted_by.map(str::to_owned)));
        let removed = self.activity.lock().remove(session_key);
        Ok(match removed {
            Some(removed) => DeletedSession {
                conversations: removed.conversations,
                messages: removed.messages,
                summaries: 0,
            },
            None => DeletedSession::default(),
        })
    }

    async fn tombstones(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, MemoryStoreError> {
        let tombstones = self.tombstones.lock();
        Ok(session_keys
            .iter()
            .filter_map(|key| {
                tombstones
                    .get(key)
                    .map(|(deleted_at, _)| (key.clone(), *deleted_at))
            })
            .collect())
    }

    async fn session_activity(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, SessionActivity>, MemoryStoreError> {
        let activity = self.activity.lock();
        Ok(session_keys
            .iter()
            .map(|key| match activity.get(key) {
                Some(found) => (key.clone(), found.clone()),
                None => (key.clone(), SessionActivity::default()),
            })
            .collect())
    }

    async fn empty_sessions(&self, _limit: u32) -> Result<Vec<String>, MemoryStoreError> {
        let tombstones = self.tombstones.lock();
        Ok(self
            .activity
            .lock()
            .iter()
            .filter(|(key, activity)| {
                activity.conversations > 0
                    && activity.messages == 0
                    && !tombstones.contains_key(*key)
            })
            .map(|(key, _)| key.clone())
            .collect())
    }

    async fn purge_tombstoned(&self, _limit: u32) -> Result<DeletedSession, MemoryStoreError> {
        Ok(DeletedSession::default())
    }

    async fn keep_session(
        &self,
        session_key: &str,
        _kept_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), MemoryStoreError> {
        self.keeps.lock().insert(session_key.to_owned(), now);
        Ok(())
    }

    async fn kept(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, MemoryStoreError> {
        let keeps = self.keeps.lock();
        Ok(session_keys
            .iter()
            .filter_map(|key| keeps.get(key).map(|kept_at| (key.clone(), *kept_at)))
            .collect())
    }

    async fn unkeep_session(&self, session_key: &str) -> Result<(), MemoryStoreError> {
        self.keeps.lock().remove(session_key);
        Ok(())
    }

    async fn trash_session(
        &self,
        session_key: &str,
        _trashed_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), MemoryStoreError> {
        self.trash
            .lock()
            .entry(session_key.to_owned())
            .or_insert(now);
        Ok(())
    }

    async fn restore_session(&self, session_key: &str) -> Result<(), MemoryStoreError> {
        self.trash.lock().remove(session_key);
        Ok(())
    }

    async fn trashed(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, MemoryStoreError> {
        let trash = self.trash.lock();
        Ok(session_keys
            .iter()
            .filter_map(|key| trash.get(key).map(|at| (key.clone(), *at)))
            .collect())
    }

    async fn purge_trashed(
        &self,
        older_than: DateTime<Utc>,
        limit: u32,
    ) -> Result<DeletedSession, MemoryStoreError> {
        let due: Vec<String> = self
            .trash
            .lock()
            .iter()
            .filter(|(_, at)| **at <= older_than)
            .zip(0..limit)
            .map(|((key, _), _)| key.clone())
            .collect();
        let mut total = DeletedSession::default();
        for key in due {
            self.trash.lock().remove(&key);
            total += self.delete_session(&key, None, older_than).await?;
        }
        Ok(total)
    }
}
