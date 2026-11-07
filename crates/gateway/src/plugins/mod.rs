mod api;
mod creator;
mod events;
pub(crate) mod fault;
mod jobs;
mod launch;
mod logs;
mod manage;
mod manifest;
mod registry;
mod rules;
mod sandbox;
mod settings;
mod supervisor;
mod tools;
mod views;

pub(crate) use api::PluginCaller;
pub(crate) use tools::tool_error;
pub(crate) use views::check as check_view;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq as _;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use wire::events::{ProtocolVersion, SessionBroadcast};
use wire::plugin::host::{
    PluginChangedNotice, PluginExit, PluginJobRun, PluginJobStatus, PluginList, PluginLogLine,
    PluginLogStream, PluginRecord, PluginState, PluginToolEntry, PluginViewsNotice,
};
use wire::plugin::manifest::{Permission, PluginJob};
use wire::plugin::process::{PluginEvent, PluginNotification, PluginSelf};
use wire::plugin::{PluginId, RouteName};

use crate::sessions::SessionRegistry;
use crate::tenancy::UserId;

pub(crate) use fault::parse;
pub use manage::start;

const NOTIFICATION_BUFFER: usize = 64;
const DEFAULT_READY: Duration = Duration::from_millis(10_000);

pub(crate) fn plugin_url(path: &str) -> String {
    format!("http://plugin/{path}")
}

#[derive(Debug, Clone)]
pub struct HostSettings {
    pub root: PathBuf,
    pub workshop: PathBuf,
    pub templates: PathBuf,
    pub node: String,
    pub inherited: Vec<(OsString, OsString)>,
    pub node_ports: Vec<u16>,
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Unconfinable(String),
    #[error(transparent)]
    Approval(#[from] kernel::plugins::ApprovalError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug)]
struct Entry {
    stored: registry::Stored,
    live: Live,
}

#[derive(Debug)]
struct Live {
    state: PluginState,
    error: Option<String>,
    last_exit: Option<PluginExit>,
    restarts: u32,
    started_at: Option<DateTime<Utc>>,
    token: Option<[u8; 32]>,
    client: Option<reqwest::Client>,
    supervisor: Option<Supervisor>,
    jobs: BTreeMap<RouteName, PluginJobRun>,
    running_jobs: BTreeSet<RouteName>,
}

impl Live {
    fn new(state: PluginState) -> Self {
        Self {
            state,
            error: None,
            last_exit: None,
            restarts: 0,
            started_at: None,
            token: None,
            client: None,
            supervisor: None,
            jobs: BTreeMap::new(),
            running_jobs: BTreeSet::new(),
        }
    }
}

#[derive(Debug)]
struct Supervisor {
    stop: CancellationToken,
    events: Arc<events::EventQueue>,
    tasks: TaskTracker,
}

struct Prepared {
    plan: launch::LaunchPlan,
    socket: PathBuf,
    ready_within: Duration,
}

#[derive(Debug)]
pub struct PluginHost {
    me: Weak<Self>,
    settings: HostSettings,
    creator: Vec<PluginToolEntry>,
    owner: OnceLock<UserId>,
    entries: Mutex<BTreeMap<PluginId, Entry>>,
    operations: tokio::sync::Mutex<()>,
    logs: logs::LogSink,
    notifications: broadcast::Sender<PluginNotification>,
    tools: watch::Sender<u64>,
    confinement: sandbox::Support,
    sessions: OnceLock<Arc<SessionRegistry>>,
    tools_key: OnceLock<[u8; 32]>,
    shutdown: CancellationToken,
    tasks: TaskTracker,
}

impl PluginHost {
    pub async fn open(settings: HostSettings) -> Result<Arc<Self>, HostError> {
        let stored = registry::load(&settings.root).await?;
        let creator = creator::entries()?;
        let tasks = TaskTracker::new();
        let shutdown = CancellationToken::new();
        let entries = stored
            .into_iter()
            .map(|(id, stored)| {
                (
                    id,
                    Entry {
                        stored,
                        live: Live::new(PluginState::Disabled),
                    },
                )
            })
            .collect();
        Ok(Arc::new_cyclic(|me| Self {
            me: me.clone(),
            logs: logs::LogSink::open(&tasks, shutdown.clone()),
            settings,
            creator,
            owner: OnceLock::new(),
            entries: Mutex::new(entries),
            operations: tokio::sync::Mutex::new(()),
            notifications: broadcast::channel(NOTIFICATION_BUFFER).0,
            tools: watch::channel(0).0,
            confinement: sandbox::support(),
            sessions: OnceLock::new(),
            tools_key: OnceLock::new(),
            shutdown,
            tasks,
        }))
    }

    fn peek<T>(&self, plugin: &PluginId, look: impl FnOnce(&Entry) -> T) -> Option<T> {
        self.entries.lock().get(plugin).map(look)
    }

    fn peek_mut<T>(&self, plugin: &PluginId, change: impl FnOnce(&mut Entry) -> T) -> Option<T> {
        self.entries.lock().get_mut(plugin).map(change)
    }

    fn read<T>(&self, plugin: &PluginId, look: impl FnOnce(&Entry) -> T) -> Result<T, HostError> {
        self.peek(plugin, look).ok_or_else(|| missing(plugin))
    }

    fn update<T>(
        &self,
        plugin: &PluginId,
        change: impl FnOnce(&mut Entry) -> T,
    ) -> Result<T, HostError> {
        self.peek_mut(plugin, change).ok_or_else(|| missing(plugin))
    }

    fn touch(&self, plugin: &PluginId, change: impl FnOnce(&mut Entry)) {
        if let Some(entry) = self.entries.lock().get_mut(plugin) {
            change(entry);
        }
    }

    fn folder(&self, plugin: &PluginId) -> PathBuf {
        self.settings.root.join(plugin.as_ref())
    }

    fn api_socket(&self) -> PathBuf {
        self.settings.root.join("api.sock")
    }

    fn tools_socket(&self) -> PathBuf {
        self.settings.root.join("tools.sock")
    }

    fn tools_token(&self) -> PathBuf {
        self.settings.root.join("tools.token")
    }

    pub(crate) fn log(&self, plugin: &PluginId, stream: PluginLogStream, text: String) {
        self.logs
            .write(&self.folder(plugin).join("logs"), plugin, stream, text);
    }

    pub(crate) async fn tail(
        &self,
        plugin: &PluginId,
        count: Option<usize>,
    ) -> Result<Vec<PluginLogLine>, HostError> {
        self.record(plugin)?;
        let count = count.unwrap_or(logs::DEFAULT_TAIL).min(logs::MAX_TAIL);
        Ok(logs::tail(&self.folder(plugin).join("logs"), count).await?)
    }

    fn changed(&self, plugin: &PluginId) {
        self.announce(&SessionBroadcast::PluginChanged(PluginChangedNotice {
            v: ProtocolVersion::V1,
            plugin: plugin.clone(),
        }));
        self.tools.send_modify(|version| *version += 1);
    }

    fn announce(&self, notice: &SessionBroadcast) {
        if let Some(sessions) = self.sessions.get() {
            sessions.announce(notice);
        }
    }

    pub(crate) fn views_changed(&self, plugin: &PluginId, views: Vec<RouteName>) {
        self.announce(&SessionBroadcast::PluginViews(PluginViewsNotice {
            v: ProtocolVersion::V1,
            plugin: plugin.clone(),
            views,
        }));
    }

    pub(crate) fn notify(&self, notification: PluginNotification) {
        if self.notifications.send(notification).is_err() {
            tracing::debug!("plug-in notification has no listening client");
        }
    }

    pub(crate) fn notifications(&self) -> broadcast::Receiver<PluginNotification> {
        self.notifications.subscribe()
    }

    pub(crate) fn node(&self) -> &str {
        &self.settings.node
    }

    fn mint(&self, plugin: &PluginId) -> Result<String, String> {
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|error| format!("token: {error}"))?;
        let token = hex::encode(secret);
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        self.touch(plugin, |entry| entry.live.token = Some(digest));
        Ok(token)
    }

    pub(crate) fn authenticate(&self, token: &str) -> Option<(PluginId, Vec<Permission>)> {
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        self.entries.lock().iter().fold(None, |found, (id, entry)| {
            let matched = entry
                .live
                .token
                .is_some_and(|held| bool::from(held.ct_eq(&digest)));
            if matched {
                Some((id.clone(), entry.stored.granted.clone()))
            } else {
                found
            }
        })
    }

    fn refuse_unconfinable(&self, requested: &[Permission]) -> Result<(), HostError> {
        sandbox::refusal(requested, self.confinement)
            .map_or(Ok(()), |refused| Err(HostError::Unconfinable(refused)))
    }

    async fn prepare(&self, plugin: &PluginId) -> Result<Prepared, String> {
        let (directory, run, requested, granted) = self
            .read(plugin, |entry| {
                (
                    entry.stored.path.clone(),
                    entry.stored.manifest.run.clone(),
                    entry.stored.manifest.permissions.clone(),
                    entry.stored.granted.clone(),
                )
            })
            .map_err(|error| error.to_string())?;
        self.refuse_unconfinable(&requested)
            .map_err(|error| error.to_string())?;
        let folder = self.folder(plugin);
        let run_dir = folder.join("run");
        let data = folder.join("data");
        let socket = run_dir.join("plugin.sock");
        reset_run_dir(&run_dir)
            .await
            .map_err(|error| format!("{}: {error}", run_dir.display()))?;
        tokio::fs::create_dir_all(&data)
            .await
            .map_err(|error| format!("{}: {error}", data.display()))?;
        let token = self.mint(plugin)?;
        let api = self.api_socket();
        let contract = launch::Contract {
            plugin,
            node: &self.settings.node,
            data: &data,
            run: &run_dir,
            socket: &socket,
            api: &api,
            token: &token,
            confine: (self.confinement != sandbox::Support::Missing).then_some(granted.as_slice()),
            node_ports: &self.settings.node_ports,
        };
        let plan = launch::plan(&run, &directory, &self.settings.inherited, &contract)
            .ok_or_else(|| "run.command is empty".to_owned())?;
        let ready_within = run
            .ready_timeout_ms
            .map_or(DEFAULT_READY, Duration::from_millis);
        self.set_state(plugin, PluginState::Starting, None);
        Ok(Prepared {
            plan,
            socket,
            ready_within,
        })
    }

    fn set_state(&self, plugin: &PluginId, state: PluginState, error: Option<String>) {
        self.touch(plugin, |entry| {
            entry.live.state = state;
            entry.live.error = error;
        });
        self.changed(plugin);
    }

    fn running(&self, plugin: &PluginId, client: reqwest::Client) {
        self.touch(plugin, |entry| {
            entry.live.state = PluginState::Running;
            entry.live.error = None;
            entry.live.client = Some(client);
            entry.live.started_at = Some(Utc::now());
        });
        self.log(plugin, PluginLogStream::Host, "ready".to_owned());
        self.changed(plugin);
    }

    fn backoff(&self, plugin: &PluginId, exit: Option<PluginExit>, reason: &str, delay: Duration) {
        let described = exit.as_ref().map(supervisor::describe);
        let error = match described {
            Some(status) => format!("{reason} ({status}); restarting in {} s", delay.as_secs()),
            None => format!("{reason}; restarting in {} s", delay.as_secs()),
        };
        let shown = error.clone();
        self.touch(plugin, |entry| {
            entry.live.state = PluginState::Backoff;
            entry.live.client = None;
            entry.live.token = None;
            entry.live.started_at = None;
            entry.live.restarts += 1;
            if exit.is_some() {
                entry.live.last_exit = exit;
            }
            entry.live.error = Some(shown);
        });
        self.log(plugin, PluginLogStream::Host, error);
        self.changed(plugin);
    }

    fn supervisor_stopped(&self, plugin: &PluginId) {
        self.touch(plugin, |entry| {
            entry.live.client = None;
            entry.live.token = None;
            entry.live.started_at = None;
        });
    }

    pub(crate) fn running_client(&self, plugin: &PluginId) -> Option<reqwest::Client> {
        self.entries.lock().get(plugin).and_then(|entry| {
            if entry.live.state == PluginState::Running {
                entry.live.client.clone()
            } else {
                None
            }
        })
    }

    pub(crate) fn proxy_client(&self, plugin: &PluginId) -> Result<reqwest::Client, HostError> {
        self.read(plugin, |_| ())?;
        self.running_client(plugin).ok_or_else(|| {
            HostError::Unavailable(format!("plug-in {} is not running", plugin.as_ref()))
        })
    }

    pub fn publish(&self, event: &PluginEvent) {
        let queues: Vec<Arc<events::EventQueue>> = self
            .entries
            .lock()
            .values()
            .filter(|entry| {
                rules::wants(&entry.stored.manifest, event)
                    && rules::event_kind(event).is_none_or(|kind| {
                        entry
                            .stored
                            .granted
                            .contains(&rules::event_permission(kind))
                    })
            })
            .filter_map(|entry| {
                entry
                    .live
                    .supervisor
                    .as_ref()
                    .map(|supervisor| Arc::clone(&supervisor.events))
            })
            .collect();
        for queue in queues {
            queue.push(event.clone());
        }
    }

    fn job(&self, plugin: &PluginId, job: &RouteName) -> Result<PluginJob, HostError> {
        self.read(plugin, |entry| {
            entry
                .stored
                .manifest
                .jobs
                .iter()
                .find(|declared| &declared.id == job)
                .cloned()
        })?
        .ok_or_else(|| HostError::NotFound(format!("no job named {}", job.as_ref())))
    }

    fn begin_job(
        &self,
        plugin: &PluginId,
        job: &RouteName,
        run: PluginJobRun,
    ) -> Result<(), HostError> {
        let began = self.update(plugin, |entry| {
            let free = entry.live.running_jobs.insert(job.clone());
            if free {
                entry.live.jobs.insert(job.clone(), run);
            }
            free
        })?;
        if began {
            Ok(())
        } else {
            Err(HostError::Conflict(format!(
                "a run of {} is in progress",
                job.as_ref()
            )))
        }
    }

    fn finish_job(&self, plugin: &PluginId, job: &RouteName, run: PluginJobRun) {
        let outcome = match run.outcome {
            Some(wire::plugin::host::PluginJobOutcome::Succeeded) => "succeeded",
            Some(wire::plugin::host::PluginJobOutcome::Failed) | None => "failed",
        };
        let line = match &run.message {
            Some(text) => format!("job {} {outcome}: {text}", job.as_ref()),
            None => format!("job {} {outcome}", job.as_ref()),
        };
        self.touch(plugin, |entry| {
            entry.live.running_jobs.remove(job);
            entry.live.jobs.insert(job.clone(), run);
        });
        self.log(plugin, PluginLogStream::Host, line);
        self.changed(plugin);
    }

    pub(crate) fn start_job(
        self: &Arc<Self>,
        plugin: &PluginId,
        job: &RouteName,
    ) -> Result<PluginJobRun, HostError> {
        jobs::start(
            self,
            plugin,
            job,
            wire::plugin::host::PluginJobTrigger::Manual,
            None,
        )
    }

    pub(crate) fn record(&self, plugin: &PluginId) -> Result<PluginRecord, HostError> {
        self.read(plugin, |entry| self.describe(plugin, entry))
    }

    pub(crate) fn list(&self) -> PluginList {
        let plugins = self
            .entries
            .lock()
            .iter()
            .map(|(id, entry)| self.describe(id, entry))
            .collect();
        PluginList {
            node: self.settings.node.clone(),
            plugins,
        }
    }

    fn describe(&self, plugin: &PluginId, entry: &Entry) -> PluginRecord {
        let manifest = &entry.stored.manifest;
        PluginRecord {
            id: plugin.clone(),
            node: self.settings.node.clone(),
            path: entry.stored.path.display().to_string(),
            manifest: manifest.clone(),
            enabled: entry.stored.enabled,
            state: entry.live.state,
            granted: entry.stored.granted.clone(),
            pending: rules::pending(&manifest.permissions, &entry.stored.granted),
            error: entry.live.error.clone(),
            last_exit: entry.live.last_exit.clone(),
            restarts: entry.live.restarts,
            started_at: entry.live.started_at,
            installed_at: entry.stored.installed_at,
            jobs: manifest
                .jobs
                .iter()
                .map(|job| PluginJobStatus {
                    id: job.id.clone(),
                    last_run: entry.live.jobs.get(&job.id).cloned(),
                })
                .collect(),
        }
    }

    pub(crate) fn own(&self, plugin: &PluginId) -> Result<PluginSelf, HostError> {
        self.read(plugin, |entry| PluginSelf {
            id: plugin.clone(),
            node: self.settings.node.clone(),
            version: entry.stored.manifest.version.clone(),
            permissions: entry.stored.granted.clone(),
            settings: settings::effective(&entry.stored),
            data_dir: self.folder(plugin).join("data").display().to_string(),
        })
    }
}

fn missing(plugin: &PluginId) -> HostError {
    HostError::NotFound(format!("no plug-in named {}", plugin.as_ref()))
}

async fn reset_run_dir(run_dir: &std::path::Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt as _;
    match tokio::fs::remove_dir_all(run_dir).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    tokio::fs::create_dir_all(run_dir).await?;
    tokio::fs::set_permissions(run_dir, std::fs::Permissions::from_mode(0o700)).await
}
