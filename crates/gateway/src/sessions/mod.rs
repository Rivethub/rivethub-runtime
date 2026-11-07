mod owners;

use std::collections::BTreeMap;

use parking_lot::RwLock;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use wire::events::{ProtocolVersion, SessionBroadcast};
use wire::gateway::topics::TopicChangedNotice;

use crate::error::ApiError;
use crate::tenancy::UserId;
use owners::OwnersFile;
pub use owners::{OWNERS_FILE_NAME, owners_file};

const NOTICE_BUFFER: usize = 256;

#[derive(Debug)]
pub struct SessionRegistry {
    owners: RwLock<BTreeMap<String, UserId>>,
    file: Option<OwnersFile>,
    node_owner: Option<UserId>,
    notices: broadcast::Sender<SessionBroadcast>,
    pub(crate) tasks: TaskTracker,
    pub(crate) shutdown: CancellationToken,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "a broadcast sender has no Default, so the notice channel is opened here"
    )
)]
impl Default for SessionRegistry {
    fn default() -> Self {
        Self {
            owners: RwLock::new(BTreeMap::new()),
            file: None,
            node_owner: None,
            notices: broadcast::channel(NOTICE_BUFFER).0,
            tasks: TaskTracker::new(),
            shutdown: CancellationToken::new(),
        }
    }
}

impl SessionRegistry {
    #[must_use]
    pub fn with_shutdown(mut self, shutdown: CancellationToken) -> Self {
        self.shutdown = shutdown;
        self
    }

    #[must_use]
    pub fn with_owners_file(mut self, path: std::path::PathBuf) -> Self {
        let (file, owners) = OwnersFile::open(path);
        self.owners = RwLock::new(owners);
        self.file = Some(file);
        self
    }

    #[must_use]
    pub fn with_node_owner(mut self, owner: UserId) -> Self {
        self.node_owner = Some(owner);
        self
    }

    fn persist_best_effort(&self) {
        if let Some(file) = &self.file {
            file.persist_best_effort(|| self.owners.read().clone());
        }
    }

    pub fn owner(&self, session: &str) -> Result<Option<UserId>, ApiError> {
        let key = session_key(session)?;
        Ok(self.owners.read().get(&key).cloned())
    }

    pub fn set(&self, session: &str, user: &UserId) -> Result<(), ApiError> {
        let key = session_key(session)?;
        let previous = self.owners.write().insert(key, user.clone());
        if previous.as_ref() != Some(user) {
            self.persist_best_effort();
        }
        Ok(())
    }

    pub fn tag_if_absent(&self, session: &str, user: &UserId) -> Result<bool, ApiError> {
        let key = session_key(session)?;
        let tagged = self.insert_if_absent(key, user);
        if tagged {
            self.persist_best_effort();
        }
        Ok(tagged)
    }

    fn insert_if_absent(&self, key: String, user: &UserId) -> bool {
        match self.owners.write().entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(user.clone());
                true
            }
            std::collections::btree_map::Entry::Occupied(_) => false,
        }
    }

    pub fn inherit(&self, child: &str, parent: &str) -> Result<bool, ApiError> {
        let inherited = self.inherit_key(&session_key(child)?, &session_key(parent)?);
        if inherited {
            self.persist_best_effort();
        }
        Ok(inherited)
    }

    fn inherit_key(&self, child: &str, parent: &str) -> bool {
        if child == parent {
            return false;
        }
        let mut owners = self.owners.write();
        if owners.contains_key(child) {
            return false;
        }
        let Some(owner) = owners.get(parent).cloned() else {
            return false;
        };
        owners.insert(child.to_owned(), owner);
        drop(owners);
        true
    }

    pub fn stamp_nested<T>(
        &self,
        rows: &[T],
        id_of: impl Fn(&T) -> &str,
        parent_of: impl Fn(&T) -> Option<&str>,
    ) {
        let mut inherited = false;
        for row in rows {
            if let Some(parent) = parent_of(row)
                && let (Ok(child), Ok(parent)) = (session_key(id_of(row)), session_key(parent))
            {
                inherited |= self.inherit_key(&child, &parent);
            }
        }
        if inherited {
            self.persist_best_effort();
        }
    }

    fn is_node_owner(&self, user: &UserId) -> bool {
        self.node_owner.as_ref() == Some(user)
    }

    fn visible_key(&self, user: &UserId, key: &str, parent: Option<&str>) -> bool {
        let owners = self.owners.read();
        let tag = owners
            .get(key)
            .or_else(|| parent.and_then(|parent| owners.get(parent)));
        let visible = match tag {
            Some(owner) => owner == user,
            None => self.is_node_owner(user),
        };
        drop(owners);
        visible
    }

    pub fn visible(
        &self,
        user: &UserId,
        session: &str,
        parent: Option<&str>,
    ) -> Result<bool, ApiError> {
        let key = session_key(session)?;
        let parent = parent.map(session_key).transpose()?;
        Ok(self.visible_key(user, &key, parent.as_deref()))
    }

    pub fn filter<T>(
        &self,
        user: &UserId,
        rows: Vec<T>,
        id_of: impl Fn(&T) -> &str,
        parent_of: impl Fn(&T) -> Option<&str>,
    ) -> Vec<T> {
        rows.into_iter()
            .filter(|row| matches!(self.visible(user, id_of(row), parent_of(row)), Ok(true)))
            .collect()
    }

    pub fn authorize_visible(&self, user: &UserId, session: &str) -> Result<String, ApiError> {
        let key = session_key(session)?;
        if self.visible_key(user, &key, None) {
            Ok(key)
        } else {
            Err(ApiError::SessionForbidden)
        }
    }

    pub fn claim(&self, user: &UserId, session: &str) -> Result<String, ApiError> {
        let key = session_key(session)?;
        let mut owners = self.owners.write();
        let claimed = match owners.get(&key) {
            Some(owner) if owner != user => return Err(ApiError::SessionForbidden),
            Some(_) => false,
            None => {
                owners.insert(key.clone(), user.clone());
                true
            }
        };
        drop(owners);
        if claimed {
            self.persist_best_effort();
        }
        Ok(key)
    }

    pub fn authorize(&self, user: &UserId, session: &str) -> Result<String, ApiError> {
        let key = session_key(session)?;
        match self.owners.read().get(&key) {
            Some(owner) if owner == user => Ok(key),
            Some(_) => Err(ApiError::SessionForbidden),
            None => Err(ApiError::NotFound("unknown session")),
        }
    }

    pub(crate) fn topic_changed(&self, topic: &impl ToString) {
        self.announce(&SessionBroadcast::TopicChanged(TopicChangedNotice {
            v: ProtocolVersion::V1,
            topic: topic.to_string(),
        }));
    }

    pub(crate) fn announce(&self, notice: &SessionBroadcast) {
        if self.notices.send(notice.clone()).is_err() {
            tracing::trace!("notice dropped; no listening client");
        }
    }

    pub fn notices(&self) -> broadcast::Receiver<SessionBroadcast> {
        self.notices.subscribe()
    }

    pub fn seed_owner(&self, user: &UserId, session: &str) -> Result<(), ApiError> {
        let key = session_key(session)?;
        let mut owners = self.owners.write();
        let same = owners.get(&key).map(|owner| owner == user);
        if let Some(same) = same {
            drop(owners);
            return if same {
                Ok(())
            } else {
                Err(ApiError::SessionForbidden)
            };
        }
        owners.insert(key, user.clone());
        drop(owners);
        self.persist_best_effort();
        Ok(())
    }

    pub async fn close(&self) {
        self.shutdown.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}

pub fn session_key(value: &str) -> Result<String, ApiError> {
    let value = if let Some((prefix, native)) = value.split_once(':')
        && prefix.parse::<wire::harness::HarnessId>().is_ok()
    {
        native
    } else {
        value
    };
    if value.is_empty()
        || value.chars().count() > 160
        || value
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')))
        || value.contains("..")
    {
        return Err(ApiError::BadRequest(format!("invalid session id: {value}")));
    }
    Ok(value.into())
}
