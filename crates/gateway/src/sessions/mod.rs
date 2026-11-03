pub mod control;
mod fanout;
mod owners;
mod view;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use parking_lot::{Mutex, RwLock};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use wire::events::{
    AgentEvent, AgentEventKind, ProtocolVersion, SessionBroadcast, SessionRemoved, Snapshot,
    StateResponse,
};
use wire::gateway::hub_state::HubStateChanged;
use wire::gateway::topics::TopicChangedNotice;

use crate::error::ApiError;
use crate::tenancy::UserId;
use owners::OwnersFile;
pub use owners::{OWNERS_FILE_NAME, owners_file};
use view::Projection;

struct Applied {
    revision: u64,
    ended: bool,
}

#[derive(Debug)]
struct Tenant {
    views: Mutex<BTreeMap<String, Projection>>,
    events: broadcast::Sender<SessionBroadcast>,
}

impl Tenant {
    fn new() -> Self {
        Self {
            views: Mutex::new(BTreeMap::new()),
            events: broadcast::channel(256).0,
        }
    }

    fn publish(&self, event: SessionBroadcast) {
        if self.events.send(event).is_err() {
            tracing::trace!("session event retained in snapshot; no live subscribers");
        }
    }
}

#[derive(Debug)]
pub struct SessionRegistry {
    owners: RwLock<BTreeMap<String, UserId>>,
    file: Option<OwnersFile>,
    node_owner: Option<UserId>,
    tenants: RwLock<BTreeMap<UserId, Arc<Tenant>>>,
    pub(crate) tasks: TaskTracker,
    pub(crate) shutdown: CancellationToken,
    eviction: Duration,
}

impl SessionRegistry {
    #[must_use]
    pub fn with_shutdown(mut self, shutdown: CancellationToken) -> Self {
        self.shutdown = shutdown;
        self
    }
    pub fn new(eviction: Duration) -> Self {
        Self {
            owners: RwLock::new(BTreeMap::new()),
            file: None,
            node_owner: None,
            tenants: RwLock::new(BTreeMap::new()),
            tasks: TaskTracker::new(),
            shutdown: CancellationToken::new(),
            eviction,
        }
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

    pub(crate) fn hub_state_changed(&self, user: &UserId, version: u64) {
        let tenant = self.tenants.read().get(user).cloned();
        if let Some(tenant) = tenant {
            tenant.publish(SessionBroadcast::HubState(HubStateChanged {
                v: ProtocolVersion::V1,
                version,
            }));
        }
    }

    pub(crate) fn topic_changed(&self, topic: &impl ToString) {
        self.announce(&SessionBroadcast::TopicChanged(TopicChangedNotice {
            v: ProtocolVersion::V1,
            topic: topic.to_string(),
        }));
    }

    pub(crate) fn announce(&self, notice: &SessionBroadcast) {
        let tenants: Vec<Arc<Tenant>> = self.tenants.read().values().cloned().collect();
        for tenant in tenants {
            tenant.publish(notice.clone());
        }
    }

    fn tenant(&self, user: &UserId) -> Arc<Tenant> {
        if let Some(tenant) = self.tenants.read().get(user) {
            return tenant.clone();
        }
        self.tenants
            .write()
            .entry(user.clone())
            .or_insert_with(|| Arc::new(Tenant::new()))
            .clone()
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

    pub fn ingest(
        self: &Arc<Self>,
        user: &UserId,
        events: Vec<AgentEvent>,
    ) -> Result<(), ApiError> {
        self.ingest_inner(user, events, false)
    }

    pub fn ingest_as_local(
        self: &Arc<Self>,
        user: &UserId,
        events: Vec<AgentEvent>,
    ) -> Result<(), ApiError> {
        self.ingest_inner(user, events, true)
    }

    fn ingest_inner(
        self: &Arc<Self>,
        user: &UserId,
        events: Vec<AgentEvent>,
        keep_foreign: bool,
    ) -> Result<(), ApiError> {
        if events.len() > 100 {
            return Err(ApiError::BadRequest("at most 100 events".into()));
        }
        let keys = events
            .iter()
            .map(|event| session_key(&event.session))
            .collect::<Result<Vec<_>, _>>()?;
        let (targets, tagged) = {
            let mut owners = self.owners.write();
            let mut targets = Vec::with_capacity(keys.len());
            for key in &keys {
                match owners.get(key) {
                    Some(owner) if owner == user || keep_foreign => targets.push(owner.clone()),
                    Some(_) => return Err(ApiError::SessionForbidden),
                    None => targets.push(user.clone()),
                }
            }
            let mut tagged = false;
            for (key, target) in keys.iter().zip(&targets) {
                if !owners.contains_key(key) {
                    owners.insert(key.clone(), target.clone());
                    tagged = true;
                }
            }
            drop(owners);
            (targets, tagged)
        };
        if tagged {
            self.persist_best_effort();
        }
        if targets.iter().all(|target| target == user) {
            self.apply_batch(user, events, keys);
            return Ok(());
        }
        for ((event, key), target) in events.into_iter().zip(keys).zip(targets) {
            self.apply_one(&target, event, key);
        }
        Ok(())
    }

    fn apply_batch(self: &Arc<Self>, user: &UserId, events: Vec<AgentEvent>, keys: Vec<String>) {
        let tenant = self.tenant(user);
        let mut views = tenant.views.lock();
        for (event, key) in events.into_iter().zip(keys) {
            let ended = Self::project(&tenant, &mut views, event, &key);
            if ended.ended {
                self.schedule_eviction(user.clone(), key, ended.revision);
            }
        }
        drop(views);
    }

    fn apply_one(self: &Arc<Self>, user: &UserId, event: AgentEvent, key: String) {
        let tenant = self.tenant(user);
        let mut views = tenant.views.lock();
        let ended = Self::project(&tenant, &mut views, event, &key);
        drop(views);
        if ended.ended {
            self.schedule_eviction(user.clone(), key, ended.revision);
        }
    }

    fn project(
        tenant: &Tenant,
        views: &mut BTreeMap<String, Projection>,
        mut event: AgentEvent,
        key: &str,
    ) -> Applied {
        key.clone_into(&mut event.session);
        let projection = views
            .entry(key.to_owned())
            .or_insert_with(|| Projection::new(key.to_owned()));
        projection.apply(&event.kind, Utc::now());
        let applied = Applied {
            revision: projection.revision,
            ended: matches!(event.kind, AgentEventKind::SessionEnd { .. }),
        };
        tenant.publish(SessionBroadcast::Event(event));
        applied
    }

    fn schedule_eviction(self: &Arc<Self>, user: UserId, key: String, revision: u64) {
        let registry = Arc::clone(self);
        self.tasks.spawn(async move {
            tokio::select! {
                () = registry.shutdown.cancelled() => {},
                () = tokio::time::sleep(registry.eviction) => registry.evict(&user, &key, revision),
            }
        });
    }

    fn evict(&self, user: &UserId, key: &str, revision: u64) {
        let tenant = self.tenant(user);
        let mut views = tenant.views.lock();
        if views
            .get(key)
            .is_some_and(|view| view.revision == revision && view.view.ended)
        {
            views.remove(key);
            tenant.publish(SessionBroadcast::Removed(SessionRemoved {
                v: ProtocolVersion::V1,
                session: key.into(),
            }));
        }
        drop(views);
    }

    pub fn remove(&self, user: &UserId, session: &str) -> Result<(), ApiError> {
        let key = self.authorize(user, session)?;
        let tenant = self.tenant(user);
        let mut views = tenant.views.lock();
        if views.remove(&key).is_none() {
            drop(views);
            return Err(ApiError::NotFound("unknown session"));
        }
        tenant.publish(SessionBroadcast::Removed(SessionRemoved {
            v: ProtocolVersion::V1,
            session: key,
        }));
        drop(views);
        Ok(())
    }

    pub fn state(&self, user: &UserId, session: &str) -> Result<StateResponse, ApiError> {
        let key = self.authorize(user, session)?;
        let tenant = self.tenant(user);
        let views = tenant.views.lock();
        let state = views
            .get(&key)
            .ok_or(ApiError::NotFound("unknown session"))?
            .view
            .clone();
        drop(views);
        Ok(StateResponse {
            session: session.to_owned(),
            state,
        })
    }

    pub fn subscribe(
        &self,
        user: &UserId,
        filter: Option<&str>,
    ) -> Result<(Snapshot, broadcast::Receiver<SessionBroadcast>), ApiError> {
        let filter = filter.map(|key| self.authorize(user, key)).transpose()?;
        let tenant = self.tenant(user);
        let views = tenant.views.lock();
        let receiver = tenant.events.subscribe();
        let mut sessions: Vec<_> = views.values().map(|view| view.summary.clone()).collect();
        sessions.sort_by(|a, b| {
            b.last_event_ts
                .cmp(&a.last_event_ts)
                .then_with(|| a.id.cmp(&b.id))
        });
        let projected = views
            .iter()
            .filter(|(key, _)| filter.as_ref().is_none_or(|filter| filter == *key))
            .map(|(key, value)| (key.clone(), value.view.clone()))
            .collect();
        drop(views);
        Ok((
            Snapshot {
                v: ProtocolVersion::V1,
                sessions,
                views: projected,
            },
            receiver,
        ))
    }

    pub fn bind_terminal(
        &self,
        user: &UserId,
        session: &str,
        terminal: &str,
    ) -> Result<(), ApiError> {
        let key = self.authorize(user, session)?;
        let tenant = self.tenant(user);
        let mut views = tenant.views.lock();
        if let Some(view) = views.get_mut(&key) {
            view.summary.pty = Some(terminal.into());
        }
        drop(views);
        Ok(())
    }

    pub fn count(&self) -> usize {
        self.tenants
            .read()
            .values()
            .map(|tenant| tenant.views.lock().len())
            .sum()
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
