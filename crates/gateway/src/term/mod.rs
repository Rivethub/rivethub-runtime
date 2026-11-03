pub mod actor;
mod chat;
mod delegated;
mod inject;
pub(crate) mod live;
mod options;
pub mod socket;
mod startup;
mod stored;
mod tasks;
mod trust;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use kernel::terminal::{TermSize, TerminalId, TerminalMux};
use kernel::topics::DeliveredVia;
use parking_lot::RwLock;
use tokio::sync::{mpsc, watch};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use wire::gateway::term::{
    Mux, PtyInfo, PtyState, TermCommand, TermConfig, TermSpawnRequest, TermSpawnResponse,
};
use wire::harness::{HarnessId, RosterCommand};
use wire::session_id::SessionId;

use crate::error::ApiError;
use crate::sessions::SessionRegistry;
use crate::tenancy::UserId;
use actor::{Command, Handle};
pub(crate) use chat::{ChatStart, Lineage};
pub use delegated::Delegate;
use options::validate_options;
pub(crate) use tasks::BYPASS_PERMISSIONS;
pub use trust::ClaudeTrust;

#[derive(Debug, Clone)]
pub struct LaunchCommand {
    pub id: String,
    pub label: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub harness: Option<HarnessId>,
    pub session: bool,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RememberedSession {
    pub id: String,
    pub session: String,
    pub command: String,
}

#[derive(Debug, Clone)]
pub struct TerminalOptions {
    pub roots: Vec<PathBuf>,
    pub default: String,
    pub commands: Vec<LaunchCommand>,
    pub max_ptys: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct Launch {
    pub(crate) command: LaunchCommand,
    pub(crate) size: TermSize,
    pub(crate) resume: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
    pub(crate) startup: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Spawned {
    pub(crate) response: TermSpawnResponse,
    pub(crate) session: SessionId,
    pub(crate) startup: Option<DeliveredVia>,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentLaunch {
    pub(crate) name: String,
    pub(crate) directory: PathBuf,
    pub(crate) harness: Option<HarnessId>,
    pub(crate) model: String,
    pub(crate) effort: String,
}

impl AgentLaunch {
    fn stored(
        &self,
        label: &str,
        value: &str,
        valid: fn(&str) -> bool,
    ) -> Result<Option<String>, ApiError> {
        if value.is_empty() {
            return Ok(None);
        }
        if valid(value) {
            Ok(Some(value.to_owned()))
        } else {
            Err(ApiError::BadRequest(format!(
                "agent \"{}\" {label} must be a 1-64 token",
                self.name
            )))
        }
    }
}

#[expect(
    missing_debug_implementations,
    reason = "kernel ports intentionally do not require Debug"
)]
pub struct TerminalService {
    pub(crate) mux: Arc<dyn TerminalMux>,
    pub(crate) registry: Arc<SessionRegistry>,
    options: TerminalOptions,
    entries: RwLock<BTreeMap<String, Handle>>,
    adopted: RwLock<BTreeMap<String, (String, PtyInfo)>>,
    pub(crate) shutdown: CancellationToken,
    tasks: TaskTracker,
    permits: Arc<tokio::sync::Semaphore>,
    pub(crate) chats: RwLock<BTreeMap<String, crate::sessions::control::ChatHandle>>,
    harness: Option<Arc<dyn kernel::harness::HarnessAdapter>>,
    summaries: RwLock<BTreeMap<String, wire::harness::HarnessSessionSummary>>,
    activity: RwLock<BTreeMap<String, kernel::preview::LiveActivity>>,
    approvals: RwLock<BTreeMap<String, Vec<wire::harness::HarnessEvent>>>,
    contexts: RwLock<BTreeMap<String, wire::harness::HarnessEvent>>,
    drivers: RwLock<BTreeMap<String, wire::harness::HarnessEvent>>,
    harness_events:
        RwLock<BTreeMap<String, tokio::sync::broadcast::Sender<wire::harness::HarnessEvent>>>,
    registry_events: tokio::sync::broadcast::Sender<live::RegistryEvent>,
    injecting: tokio::sync::Mutex<()>,
    claude_trust: Option<ClaudeTrust>,
    session_lister: Option<Arc<dyn kernel::transcript::SessionLister>>,
    startup_dir: Option<PathBuf>,
    plugins: std::sync::OnceLock<Arc<crate::plugins::PluginHost>>,
    delegates: BTreeMap<String, Delegate>,
    requested: RwLock<BTreeMap<String, SessionId>>,
}

impl TerminalService {
    #[must_use]
    pub fn with_shutdown(mut self, shutdown: CancellationToken) -> Self {
        self.shutdown = shutdown;
        self
    }
    pub async fn new(
        mux: Arc<dyn TerminalMux>,
        registry: Arc<SessionRegistry>,
        mut options: TerminalOptions,
    ) -> Result<Self, ApiError> {
        options = tokio::task::spawn_blocking(move || validate_options(options))
            .await
            .map_err(ApiError::Task)??;
        Ok(Self {
            mux,
            registry,
            permits: Arc::new(tokio::sync::Semaphore::new(options.max_ptys)),
            options,
            entries: RwLock::new(BTreeMap::new()),
            adopted: RwLock::new(BTreeMap::new()),
            shutdown: CancellationToken::new(),
            tasks: TaskTracker::new(),
            chats: RwLock::new(BTreeMap::new()),
            harness: None,
            summaries: RwLock::new(BTreeMap::new()),
            activity: RwLock::new(BTreeMap::new()),
            approvals: RwLock::new(BTreeMap::new()),
            contexts: RwLock::new(BTreeMap::new()),
            drivers: RwLock::new(BTreeMap::new()),
            harness_events: RwLock::new(BTreeMap::new()),
            registry_events: tokio::sync::broadcast::channel(live::REGISTRY_EVENT_BUFFER).0,
            injecting: tokio::sync::Mutex::new(()),
            claude_trust: None,
            session_lister: None,
            startup_dir: None,
            plugins: std::sync::OnceLock::new(),
            delegates: BTreeMap::new(),
            requested: RwLock::new(BTreeMap::new()),
        })
    }

    #[must_use]
    pub fn with_harness(mut self, harness: Arc<dyn kernel::harness::HarnessAdapter>) -> Self {
        self.harness = Some(harness);
        self
    }

    #[must_use]
    pub fn with_claude_trust(mut self, trust: ClaudeTrust) -> Self {
        self.claude_trust = Some(trust);
        self
    }

    pub(crate) async fn trust_claude_directory(
        &self,
        command: &LaunchCommand,
    ) -> Result<(), ApiError> {
        match &self.claude_trust {
            Some(trust) if command.harness == Some(HarnessId::ClaudeCode) => {
                trust.apply(&self.options.roots, &command.cwd).await
            }
            _ => Ok(()),
        }
    }

    #[must_use]
    pub fn adapter(&self) -> Option<Arc<dyn kernel::harness::HarnessAdapter>> {
        self.harness.clone()
    }

    pub(crate) fn launch(
        &self,
        request: TermSpawnRequest,
        preset: Option<AgentLaunch>,
    ) -> Result<Launch, ApiError> {
        if request.agent_id.is_some() && preset.is_none() {
            return Err(ApiError::NotFound("agent not found"));
        }
        let key = match (request.command, &preset) {
            (Some(key), _) => key,
            (None, None) => self.options.default.clone(),
            (None, Some(preset)) => match preset.harness {
                Some(harness) => RosterCommand::from(harness).to_string(),
                None => {
                    return Err(ApiError::BadRequest(
                        "agent has no harness and no command was given".into(),
                    ));
                }
            },
        };
        let mut command = self
            .options
            .commands
            .iter()
            .find(|command| command.id == key)
            .cloned()
            .ok_or_else(|| ApiError::UnknownCommand(key))?;
        token("model", request.model.as_deref(), valid_model)?;
        token("effort", request.effort.as_deref(), valid_token)?;
        let (model, effort) = match preset {
            Some(preset) => {
                let model = match request.model {
                    Some(model) => Some(model),
                    None => preset.stored("model", &preset.model, valid_model)?,
                };
                let effort = match request.effort {
                    Some(effort) => Some(effort),
                    None => preset.stored("effort", &preset.effort, valid_token)?,
                };
                command.cwd = preset.directory;
                (model, effort)
            }
            None => (request.model, request.effort),
        };
        let cols = match request.cols {
            Some(cols) => cols.clamp(20, 400),
            None => 80,
        };
        let rows = match request.rows {
            Some(rows) => rows.clamp(10, 200),
            None => 24,
        };
        let resume = request.resume.map(|value| native_id(&value).to_owned());
        Ok(Launch {
            command,
            size: TermSize { cols, rows },
            resume,
            model,
            effort,
            startup: None,
        })
    }

    pub async fn spawn(
        self: &Arc<Self>,
        user: &UserId,
        request: TermSpawnRequest,
    ) -> Result<TermSpawnResponse, ApiError> {
        Ok(self
            .spawn_from(user, request, None, None, None)
            .await?
            .response)
    }

    pub(crate) async fn spawn_from(
        self: &Arc<Self>,
        user: &UserId,
        request: TermSpawnRequest,
        preset: Option<AgentLaunch>,
        startup: Option<String>,
        workdir: Option<PathBuf>,
    ) -> Result<Spawned, ApiError> {
        let session = match (&request.session, &request.resume) {
            (Some(session), Some(resume)) => {
                if crate::sessions::session_key(session)? != crate::sessions::session_key(resume)? {
                    return Err(ApiError::Conflict(
                        "resume must retain the registered session id".into(),
                    ));
                }
                self.registry.claim(user, resume)?
            }
            (None, Some(resume)) => self.registry.claim(user, resume)?,
            (Some(session), None) => session.clone(),
            (None, None) => format!("rivethub-{}", uuid::Uuid::new_v4()),
        };
        let session = self.registry.claim(user, &session)?;
        let mut launch = Launch {
            startup,
            ..self.launch(request, preset)?
        };
        if let Some(workdir) = workdir {
            launch.command.cwd = workdir;
        }
        let port_session = Self::port_session(&launch.command, &session)?;
        let chat = self.chats.read().get(&session).cloned();
        if let Some(chat) = chat {
            return Ok(Spawned {
                response: chat.terminal(launch).await?,
                session: port_session,
                startup: None,
            });
        }
        let chat_capable = launch.command.harness.is_some();
        let spawned = self.spawn_raw(user, &session, launch).await?;
        if chat_capable && let Some(harness) = &self.harness {
            let mut chats = self.chats.write();
            if let std::collections::btree_map::Entry::Vacant(entry) = chats.entry(session) {
                let chat = crate::sessions::control::ChatActor::start(
                    self,
                    harness.clone(),
                    user.clone(),
                    port_session,
                    None,
                )?;
                entry.insert(chat);
            }
            drop(chats);
        }
        Ok(spawned)
    }

    pub(crate) async fn spawn_raw(
        self: &Arc<Self>,
        user: &UserId,
        session: &str,
        launch: Launch,
    ) -> Result<Spawned, ApiError> {
        let handle = {
            let mut entries = self.entries.write();
            entries
                .entry(session.to_owned())
                .or_insert_with(|| {
                    let (commands, receiver) = mpsc::channel(64);
                    let (status, state) = watch::channel(None);
                    let (owner, ownership) = watch::channel(None);
                    let handle = Handle {
                        commands,
                        state,
                        ownership,
                    };
                    let actor = actor::TerminalActor::new(
                        self.clone(),
                        user.clone(),
                        session.into(),
                        status,
                        owner,
                    );
                    self.tasks.spawn(async move {
                        if let Err(error) = actor.run(receiver).await {
                            tracing::error!(%error, "terminal actor stopped");
                        }
                    });
                    handle
                })
                .clone()
        };
        handle.ask(|reply| Command::Spawn { launch, reply }).await
    }

    pub(crate) fn find(
        &self,
        user: &UserId,
        id: Option<&str>,
        session: Option<&str>,
    ) -> Result<(String, Handle), ApiError> {
        let entries = self.entries.read();
        let found = if let Some(id) = id {
            entries.iter().find(|(_, handle)| {
                handle
                    .state
                    .borrow()
                    .as_ref()
                    .is_some_and(|state| state.id == id)
            })
        } else if let Some(session) = session {
            let key = self.registry.authorize(user, session)?;
            entries.get_key_value(&key)
        } else {
            None
        };
        let (key, handle) = found.ok_or(ApiError::NotFound("unknown pty"))?;
        self.registry.authorize(user, key)?;
        Ok((key.clone(), handle.clone()))
    }

    pub fn remember(&self, user: &UserId, adopted: RememberedSession) -> Result<(), ApiError> {
        let key = self.registry.claim(user, &adopted.session)?;
        let info = PtyInfo {
            id: adopted.id.clone(),
            session: key.clone(),
            command: adopted.command,
            state: PtyState::Running,
            pid: None,
            attached: 0,
            exit_code: None,
            created_at: Utc::now(),
            last_output_ts: None,
            cols: None,
            rows: None,
            mux: Some(Mux::Herdr),
            persisted: Some(true),
            reattached: Some(true),
            attach: None,
        };
        self.adopted.write().insert(adopted.id, (key, info));
        Ok(())
    }

    pub fn list(&self, user: &UserId) -> Vec<PtyInfo> {
        let mut listed: Vec<PtyInfo> = self
            .entries
            .read()
            .iter()
            .filter(|(key, _)| self.registry.authorize(user, key).is_ok())
            .filter_map(|(_, handle)| handle.state.borrow().clone())
            .collect();
        let seen: std::collections::BTreeSet<String> =
            listed.iter().map(|info| info.id.clone()).collect();
        for (key, info) in self.adopted.read().values() {
            if seen.contains(&info.id) {
                continue;
            }
            if self.registry.authorize(user, key).is_ok() {
                listed.push(info.clone());
            }
        }
        listed
    }

    pub fn config(&self, user: &UserId) -> TermConfig {
        TermConfig {
            enabled: true,
            default: self.options.default.clone(),
            max_ptys: self.options.max_ptys as u64,
            active: self
                .list(user)
                .iter()
                .filter(|pty| pty.state == wire::gateway::term::PtyState::Running)
                .count() as u64,
            commands: self
                .options
                .commands
                .iter()
                .map(|command| TermCommand {
                    id: command.id.clone(),
                    label: command.label.clone(),
                    session: command.session,
                })
                .collect(),
        }
    }

    pub async fn kill(&self, user: &UserId, id: &str) -> Result<(), ApiError> {
        match self.find(user, Some(id), None) {
            Ok((_, handle)) => handle.ask(|reply| Command::Kill { reply }).await,
            Err(ApiError::NotFound(_)) => self.kill_remembered(user, id).await,
            Err(error) => Err(error),
        }
    }

    async fn kill_remembered(&self, user: &UserId, id: &str) -> Result<(), ApiError> {
        let key = self.adopted.read().get(id).map(|(key, _)| key.clone());
        let Some(key) = key else {
            return Err(ApiError::NotFound("unknown pty"));
        };
        self.registry.authorize(user, &key)?;
        self.mux.kill(&TerminalId::new(id)).await?;
        self.adopted.write().remove(id);
        Ok(())
    }

    pub async fn remove_session(&self, user: &UserId, session: &str) -> Result<(), ApiError> {
        let key = self.registry.authorize(user, session)?;
        let chat = self.chats.write().remove(&key);
        self.summaries.write().remove(&key);
        self.contexts.write().remove(&key);
        self.drivers.write().remove(&key);
        if let Some(chat) = chat {
            chat.close().await?;
        }
        self.remove_terminal(user, session).await
    }

    pub(crate) async fn remove_terminal(
        &self,
        user: &UserId,
        session: &str,
    ) -> Result<(), ApiError> {
        let key = self.registry.authorize(user, session)?;
        let handle = self.entries.read().get(&key).cloned();
        if let Some(handle) = handle {
            let state = handle.state.borrow().clone();
            if let Some(state) = state {
                self.kill(user, &state.id).await?;
            }
        }
        Ok(())
    }

    pub async fn close(&self) {
        self.shutdown.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }

    pub(crate) fn port_session(
        command: &LaunchCommand,
        session: &str,
    ) -> Result<SessionId, ApiError> {
        if let Some((prefix, _)) = session.split_once(':') {
            let harness = prefix
                .parse::<HarnessId>()
                .map_err(|error| ApiError::BadRequest(error.to_string()))?;
            if command.harness.is_some_and(|selected| selected != harness) {
                return Err(ApiError::Conflict(
                    "session harness does not match terminal command".into(),
                ));
            }
            return session
                .parse()
                .map_err(|error: wire::session_id::SessionIdError| {
                    ApiError::BadRequest(error.to_string())
                });
        }
        Ok(match command.harness {
            Some(harness) => SessionId::new(harness, session),
            None => SessionId::new(HarnessId::Cowork, session),
        })
    }
}

fn token(label: &str, value: Option<&str>, valid: fn(&str) -> bool) -> Result<(), ApiError> {
    match value {
        Some(value) if !valid(value) => Err(ApiError::BadRequest(format!(
            "{label} must be a 1-64 token"
        ))),
        _ => Ok(()),
    }
}

fn valid_token(value: &str) -> bool {
    value.chars().count() <= 64 && token_part(value)
}

fn valid_model(value: &str) -> bool {
    value.chars().count() <= 64 && value.split('/').all(token_part)
}

fn token_part(value: &str) -> bool {
    !value.is_empty()
        && !value.contains("..")
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

fn native_id(value: &str) -> &str {
    match value.split_once(':') {
        Some((prefix, native)) if prefix.parse::<HarnessId>().is_ok() => native,
        _ => value,
    }
}
