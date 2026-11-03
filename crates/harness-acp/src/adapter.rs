use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use agent_client_protocol::schema::v1 as acp;
use async_trait::async_trait;
use config::Config;
use kernel::harness::ApprovalDecision;
use kernel::harness::HarnessAdapter;
use kernel::harness::HarnessAdapterError;
use kernel::harness::HarnessCommands;
use kernel::harness::SessionEvent;
use kernel::harness::SessionOption;
use kernel::harness::SessionSpec;
use kernel::harness::SessionSwitches;
use kernel::harness::SteerOutcome;
use kernel::harness::SystemPromptSupport;
use kernel::harness::TurnInput;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use crate::connection::AgentConnection;
use crate::connection::SessionCommand;
use crate::connection::port_failure;
use crate::connection::request;
use crate::error::AcpError;
use crate::error::port;
use crate::identity;
use crate::launch::AcpCommand;
use crate::launch::LaunchTable;
use crate::launch::probe;
use crate::launch::resolve_program;
use crate::options::SessionOptions;
use crate::permissions::PermissionRegistry;
use crate::sheets::SheetInput;
use crate::sheets::sheet_inputs;

#[derive(Debug)]
struct SessionEntry {
    tasks: tokio_util::task::TaskTracker,
    reaped: Option<tokio::sync::oneshot::Receiver<Result<(), std::io::Error>>>,
    commands: mpsc::Sender<SessionCommand>,
    acp_session: acp::SessionId,
    permissions: Arc<PermissionRegistry>,
    options: Arc<SessionOptions>,
    turn_end: mpsc::UnboundedSender<crate::context::TurnEnd>,
    events: Option<Receiver<SessionEvent>>,
    exited: Arc<AtomicBool>,
    shutdown: CancellationToken,
    images: bool,
    steering: bool,
}

impl Drop for SessionEntry {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

#[derive(Clone, Debug)]
struct SessionSnapshot {
    commands: mpsc::Sender<SessionCommand>,
    acp_session: acp::SessionId,
    permissions: Arc<PermissionRegistry>,
    options: Arc<SessionOptions>,
    turn_end: mpsc::UnboundedSender<crate::context::TurnEnd>,
    exited: Arc<AtomicBool>,
    images: bool,
    steering: bool,
}

#[derive(Debug)]
pub struct AcpHarnessAdapter {
    launch: LaunchTable,
    default_working_dir: Option<PathBuf>,
    sessions: Mutex<BTreeMap<SessionId, SessionEntry>>,
    sheets: BTreeMap<HarnessId, SheetInput>,
    search_paths: Vec<PathBuf>,
    discoveries: Arc<crate::discovery::Discoveries>,
    discovery_dir: Option<PathBuf>,
    unsaved: bool,
}

impl AcpHarnessAdapter {
    pub(crate) fn headless(
        config: &Config,
        harness: HarnessId,
        command: AcpCommand,
        search_paths: Vec<PathBuf>,
    ) -> Self {
        let mut adapter = Self::new(config).with_search_paths(search_paths);
        adapter.launch = LaunchTable::from_entries([(harness, crate::AcpLaunch::Native(command))]);
        adapter.unsaved = true;
        adapter
    }

    pub(crate) fn advertise(
        self,
        harness: HarnessId,
        advertisement: crate::discovery::Advertisement,
    ) -> Self {
        self.discoveries.record(harness, advertisement);
        self
    }

    #[must_use]
    pub fn new(config: &Config) -> Self {
        Self {
            launch: LaunchTable::from_config(config),
            default_working_dir: config
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.workspace.clone()),
            sessions: Mutex::new(BTreeMap::new()),
            sheets: sheet_inputs(config),
            search_paths: Vec::new(),
            discoveries: Arc::default(),
            discovery_dir: None,
            unsaved: false,
        }
    }

    #[must_use]
    pub fn with_launch_table(launch: LaunchTable) -> Self {
        Self {
            launch,
            default_working_dir: None,
            sessions: Mutex::new(BTreeMap::new()),
            sheets: BTreeMap::new(),
            search_paths: Vec::new(),
            discoveries: Arc::default(),
            discovery_dir: None,
            unsaved: false,
        }
    }

    #[must_use]
    pub fn with_discovery_dir(mut self, directory: PathBuf) -> Self {
        self.discovery_dir = Some(directory);
        self
    }

    #[must_use]
    pub fn with_search_paths(mut self, search_paths: Vec<PathBuf>) -> Self {
        self.search_paths = search_paths;
        self
    }

    fn snapshot(&self, session: &SessionId) -> Result<SessionSnapshot, HarnessAdapterError> {
        let sessions = self.sessions.lock();
        let snapshot = sessions.get(session).map(|entry| SessionSnapshot {
            commands: entry.commands.clone(),
            acp_session: entry.acp_session.clone(),
            permissions: entry.permissions.clone(),
            options: Arc::clone(&entry.options),
            turn_end: entry.turn_end.clone(),
            exited: entry.exited.clone(),
            images: entry.images,
            steering: entry.steering,
        });
        drop(sessions);
        snapshot.ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))
    }

    fn insert(&self, session: SessionId, agent: AgentConnection) {
        let mut sessions = self.sessions.lock();
        sessions.insert(
            session,
            SessionEntry {
                tasks: agent.tasks,
                reaped: Some(agent.reaped),
                commands: agent.commands,
                acp_session: agent.acp_session,
                permissions: agent.permissions,
                options: agent.options,
                turn_end: agent.turn_end,
                events: Some(agent.events_rx),
                exited: agent.exited,
                shutdown: agent.shutdown,
                images: agent.capabilities.prompt_capabilities.image,
                steering: agent.steering,
            },
        );
    }

    fn session_options(&self, session: &SessionId) -> Arc<SessionOptions> {
        let live = self
            .sessions
            .lock()
            .get(session)
            .map(|entry| Arc::clone(&entry.options));
        live.or_else(|| {
            self.discoveries
                .advertised(session.harness())
                .map(|advertised| advertised.options())
        })
        .unwrap_or_default()
    }

    fn remove(&self, session: &SessionId) {
        let mut sessions = self.sessions.lock();
        sessions.remove(session);
    }

    fn working_dir(
        &self,
        spec: &SessionSpec,
        command: &AcpCommand,
    ) -> Result<PathBuf, HarnessAdapterError> {
        spec.working_dir
            .clone()
            .or_else(|| command.working_dir.clone())
            .or_else(|| self.default_working_dir.clone())
            .ok_or_else(|| port(AcpError::WorkingDir))
    }

    fn launch_command(&self, harness: HarnessId) -> Result<AcpCommand, HarnessAdapterError> {
        let command = self.launch.verified_command(harness).map_err(port)?;
        self.located(command)
    }

    fn session_command(
        &self,
        harness: HarnessId,
        session: &SessionId,
    ) -> Result<AcpCommand, HarnessAdapterError> {
        let mut command = self.launch_command(harness)?;
        command
            .env
            .push((SESSION_KEY.to_owned(), session.as_str().to_owned()));
        Ok(command)
    }

    fn located(&self, mut command: AcpCommand) -> Result<AcpCommand, HarnessAdapterError> {
        if self.search_paths.is_empty() {
            return Ok(command);
        }
        let found = resolve_program(&command.program, &self.search_paths).map_err(|error| {
            HarnessAdapterError::Failed {
                message: "install probe failed".into(),
                source: Box::new(error),
            }
        })?;
        if let Some(path) = found {
            command.program = path;
        }
        Ok(command)
    }

    fn resume_dir(&self, command: &AcpCommand) -> Result<PathBuf, HarnessAdapterError> {
        command
            .working_dir
            .clone()
            .or_else(|| self.default_working_dir.clone())
            .ok_or_else(|| port(AcpError::WorkingDir))
    }

    fn discovery_target(
        &self,
        harness: HarnessId,
    ) -> Result<Option<(HarnessId, AcpCommand, PathBuf)>, HarnessAdapterError> {
        let launch = self.launch.launch(harness).map_err(port)?;
        if !launch.is_verified() {
            return Ok(None);
        }
        let mut command = self.located(launch.command().clone())?;
        command
            .env
            .push((config::MODEL_RUN_VARIABLE.to_owned(), "1".to_owned()));
        let cwd = self
            .discovery_dir
            .as_ref()
            .or(command.working_dir.as_ref())
            .or(self.default_working_dir.as_ref())
            .cloned();
        Ok(cwd.map(|cwd| (harness, command, cwd)))
    }

    fn requested_session(spec: &SessionSpec) -> Result<(), HarnessAdapterError> {
        if spec.session.harness() == spec.harness {
            Ok(())
        } else {
            Err(HarnessAdapterError::InvalidSession(spec.session.clone()))
        }
    }
}

const SESSION_KEY: &str = "RIVETHUB_SESSION_KEY";

#[async_trait]
impl HarnessAdapter for AcpHarnessAdapter {
    async fn release(&self, session: &SessionId) -> Result<(), HarnessAdapterError> {
        let mut entry = self
            .sessions
            .lock()
            .remove(session)
            .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?;
        entry.shutdown.cancel();
        let reaped = entry
            .reaped
            .take()
            .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?;
        let outcome = reaped
            .await
            .map_err(|source| HarnessAdapterError::Failed {
                message: "child reaper closed".into(),
                source: Box::new(source),
            })?
            .map_err(|source| HarnessAdapterError::Failed {
                message: "child release failed".into(),
                source: Box::new(source),
            });
        entry.tasks.wait().await;
        outcome
    }
    fn system_prompt(&self, harness: HarnessId) -> SystemPromptSupport {
        crate::launch::system_prompt(harness)
    }

    async fn new_session(&self, spec: SessionSpec) -> Result<SessionId, HarnessAdapterError> {
        Self::requested_session(&spec)?;
        let command = self.session_command(spec.harness, &spec.session)?;
        let working_dir = self.working_dir(&spec, &command)?;
        let mut meta = match spec.startup_context.as_deref() {
            Some(text) => crate::launch::startup_meta(spec.harness, text).map_err(port)?,
            None => None,
        };
        if self.unsaved {
            meta = crate::launch::unsaved(spec.harness, meta).map_err(port)?;
        }
        let agent = crate::connection::start_new(&command, &working_dir, meta)
            .await
            .map_err(port)?;
        let native =
            match identity::NativeSession::parse(spec.harness, agent.acp_session.0.as_ref()) {
                Ok(native) => native,
                Err(error) => {
                    agent.shutdown.cancel();
                    return Err(port(error));
                }
            };
        let canonical = native.canonical();
        {
            let sessions = self.sessions.lock();
            if sessions.contains_key(&canonical) {
                agent.shutdown.cancel();
                return Err(port(AcpError::SessionActive { session: canonical }));
            }
        }
        self.discoveries.remember(
            spec.harness,
            crate::discovery::Advertisement::from_agent(&agent),
        );
        self.insert(canonical.clone(), agent);
        if let Some(model) = spec.model
            && let Err(error) = self
                .set_option(&canonical, SessionOption::Model(model))
                .await
        {
            self.remove(&canonical);
            return Err(error);
        }
        if let Some(effort) = spec.effort
            && self.switches(&canonical).effort
            && let Err(error) = self
                .set_option(&canonical, SessionOption::Effort(effort))
                .await
        {
            self.remove(&canonical);
            return Err(error);
        }
        Ok(canonical)
    }

    async fn resume(
        &self,
        session: &SessionId,
        cwd: Option<&std::path::Path>,
    ) -> Result<(), HarnessAdapterError> {
        let native = identity::from_canonical(session).map_err(port)?;
        let command = self.session_command(native.harness(), session)?;
        {
            let sessions = self.sessions.lock();
            if sessions.contains_key(session) {
                return Err(port(AcpError::SessionActive {
                    session: session.clone(),
                }));
            }
        }
        let working_dir = cwd.map_or_else(|| self.resume_dir(&command), |cwd| Ok(cwd.into()))?;
        let protocol_session = acp::SessionId::new(native.native());
        let agent = crate::connection::start_loaded(&command, &working_dir, &protocol_session)
            .await
            .map_err(port)?;
        if let Some(known) = self.discoveries.advertised(native.harness()) {
            agent.context.adopt(known.context());
        }
        self.discoveries.remember(
            native.harness(),
            crate::discovery::Advertisement::from_agent(&agent),
        );
        self.insert(session.clone(), agent);
        Ok(())
    }

    async fn prompt(
        &self,
        session: &SessionId,
        input: TurnInput,
    ) -> Result<(), HarnessAdapterError> {
        let snapshot = self.snapshot(session)?;
        if snapshot.exited.load(Ordering::Acquire) {
            return Err(port(AcpError::Closed {
                method: "session/prompt",
            }));
        }
        let content = crate::prompt::content(input, snapshot.images)
            .await
            .map_err(port)?;
        let responded = request(&snapshot.commands, |reply| SessionCommand::Prompt {
            acp_session: snapshot.acp_session.clone(),
            content,
            reply,
        })
        .await;
        let ended = match responded {
            Ok(response) => {
                tracing::debug!(stop = ?response.stop_reason, "turn ended");
                crate::context::TurnEnd {
                    usage: crate::events::turn_usage(&response).map_err(port)?,
                    last_call: crate::context::last_call(&response).map_err(port)?,
                }
            }
            Err(failure) => {
                let error = if snapshot.exited.load(Ordering::Acquire) {
                    AcpError::Closed {
                        method: "session/prompt",
                    }
                } else {
                    port_failure(failure, "session/prompt")
                };
                return Err(port(error));
            }
        };
        if snapshot.turn_end.send(ended).is_err() {
            tracing::debug!("turn end had no listener");
        }
        Ok(())
    }

    async fn steer(
        &self,
        session: &SessionId,
        input: TurnInput,
    ) -> Result<SteerOutcome, HarnessAdapterError> {
        let snapshot = self.snapshot(session)?;
        if !snapshot.steering {
            return Err(HarnessAdapterError::SteeringUnsupported);
        }
        let content = crate::prompt::content(input, snapshot.images)
            .await
            .map_err(port)?;
        request(&snapshot.commands, |reply| SessionCommand::Steer {
            acp_session: snapshot.acp_session.clone(),
            content,
            reply,
        })
        .await
        .map_err(|failure| port(port_failure(failure, crate::steering::METHOD)))?
        .outcome()
        .map_err(port)
    }

    async fn cancel(&self, session: &SessionId) -> Result<(), HarnessAdapterError> {
        let snapshot = self.snapshot(session)?;
        snapshot.permissions.cancel_all();
        snapshot
            .commands
            .send(SessionCommand::Cancel {
                acp_session: snapshot.acp_session,
            })
            .await
            .map_err(|_| {
                port(AcpError::Closed {
                    method: "session/cancel",
                })
            })
    }

    async fn answer_permission(
        &self,
        session: &SessionId,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessAdapterError> {
        let snapshot = self.snapshot(session)?;
        snapshot
            .permissions
            .answer(request_id, decision)
            .map_err(port)
    }

    async fn set_option(
        &self,
        session: &SessionId,
        option: SessionOption,
    ) -> Result<(), HarnessAdapterError> {
        let snapshot = self.snapshot(session)?;
        let selected = snapshot.options.select(&option).map_err(port)?;
        let response = request(&snapshot.commands, |reply| SessionCommand::SetConfig {
            acp_session: snapshot.acp_session.clone(),
            config_id: selected.config_id,
            value: selected.value,
            reply,
        })
        .await
        .map_err(|failure| port(port_failure(failure, "session/set_config_option")))?;
        snapshot.options.replace(response.config_options);
        Ok(())
    }

    fn switches(&self, session: &SessionId) -> SessionSwitches {
        self.session_options(session).switches()
    }

    fn mode(&self, session: &SessionId) -> Option<String> {
        self.session_options(session).mode()
    }

    fn admit(
        &self,
        session: &SessionId,
        option: &SessionOption,
    ) -> Result<(), HarnessAdapterError> {
        self.session_options(session)
            .select(option)
            .map(drop)
            .map_err(port)
    }

    async fn events(
        &self,
        session: &SessionId,
    ) -> Result<Receiver<SessionEvent>, HarnessAdapterError> {
        let mut sessions = self.sessions.lock();
        let taken = sessions.get_mut(session).map(|entry| entry.events.take());
        drop(sessions);
        let receiver = taken.ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?;
        receiver.ok_or_else(|| HarnessAdapterError::EventStreamTaken(session.clone()))
    }

    async fn offerings(
        &self,
    ) -> Result<Vec<kernel::harness::HarnessOffering>, HarnessAdapterError> {
        let facts = self.launch.facts();
        let search = self.search_paths.clone();
        let installed = tokio::task::spawn_blocking(move || probe(&facts, &search))
            .await
            .map_err(|error| HarnessAdapterError::Failed {
                message: "install probe failed".into(),
                source: Box::new(error),
            })?
            .map_err(|error| HarnessAdapterError::Failed {
                message: "install probe failed".into(),
                source: Box::new(error),
            })?;
        let mut offerings = Vec::new();
        let mut pending = Vec::new();
        for (fact, present) in installed {
            if !present {
                continue;
            }
            offerings.push(
                crate::sheets::offering(
                    fact.harness,
                    fact.verified_chat,
                    self.sheets.get(&fact.harness),
                )
                .map_err(|error| HarnessAdapterError::Failed {
                    message: error.to_string(),
                    source: Box::new(error),
                })?,
            );
            if self.discoveries.advertised(fact.harness).is_none()
                && let Some(target) = self.discovery_target(fact.harness)?
            {
                pending.push(target);
            }
        }
        let attempted: Vec<HarnessId> = pending.iter().map(|(harness, ..)| *harness).collect();
        self.discoveries.start_due(pending);
        for offering in &mut offerings {
            match self.discoveries.advertised(offering.harness_id) {
                Some(advertised) => advertised
                    .apply(offering, self.sheets.get(&offering.harness_id))
                    .map_err(port)?,
                None if attempted.contains(&offering.harness_id) => offering.chat = false,
                None => {}
            }
        }
        Ok(offerings)
    }

    async fn commands(&self) -> Result<Vec<HarnessCommands>, HarnessAdapterError> {
        let offered = self.offerings().await?;
        Ok(offered
            .iter()
            .filter_map(|offering| {
                self.discoveries
                    .commands(offering.harness_id)
                    .map(|commands| HarnessCommands {
                        harness: offering.harness_id,
                        commands,
                    })
            })
            .collect())
    }
}
