use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use agent_client_protocol::Agent;
use agent_client_protocol::ConnectionTo;
use agent_client_protocol::schema::v1 as acp;
use kernel::harness::Command;
use kernel::harness::HarnessModelsSource;
use kernel::harness::HarnessOffering;
use kernel::harness::SessionEvent;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::mpsc;
use wire::harness::HarnessId;

use crate::connection::AgentConnection;
use crate::connection::SessionCommand;
use crate::connection::port_failure;
use crate::connection::request;
use crate::error::AcpError;
use crate::launch::AcpCommand;
use crate::options::SessionOptions;

#[derive(Debug, Clone)]
pub(crate) struct Advertisement {
    capabilities: acp::AgentCapabilities,
    options: Arc<SessionOptions>,
    commands: Vec<Command>,
}

impl Advertisement {
    pub(crate) fn from_agent(agent: &AgentConnection) -> Self {
        Self {
            capabilities: agent.capabilities.clone(),
            options: Arc::clone(&agent.options),
            commands: Vec::new(),
        }
    }

    pub(crate) fn options(&self) -> Arc<SessionOptions> {
        Arc::clone(&self.options)
    }

    pub(crate) fn commands(&self) -> &[Command] {
        &self.commands
    }

    fn observe(&mut self, event: SessionEvent) {
        if let SessionEvent::AvailableCommands(commands) = event {
            self.commands = commands;
        }
    }

    async fn observing<T>(
        &mut self,
        events: &mut mpsc::Receiver<SessionEvent>,
        work: impl Future<Output = T>,
    ) -> T {
        tokio::pin!(work);
        loop {
            tokio::select! {
                biased;
                done = &mut work => return done,
                Some(event) = events.recv() => self.observe(event),
            }
        }
    }

    pub(crate) fn apply(
        &self,
        offering: &mut HarnessOffering,
        input: Option<&crate::sheets::SheetInput>,
    ) -> Result<(), AcpError> {
        offering.chat = true;
        offering.capabilities.control.resume = self.capabilities.load_session;
        offering.capabilities.control.interrupt = true;
        offering.capabilities.control.approvals = true;
        offering.capabilities.observation.live_stream = true;
        offering.capabilities.observation.drive = Some(true);
        offering.capabilities.observation.list_sessions =
            self.capabilities.session_capabilities.list.is_some();
        if let Some(mut models) = self.options.models()? {
            let configured = input
                .and_then(|input| input.models.as_ref())
                .filter(|models| !models.is_empty());
            let mode = input.and_then(|input| input.mode);
            let source = if let Some(configured) =
                configured.filter(|_| mode != Some(config::ModelsMode::Discover))
            {
                let configured_models = offering
                    .capabilities
                    .models
                    .take()
                    .into_iter()
                    .flatten()
                    .filter(|model| configured.contains(&model.id));
                if mode == Some(config::ModelsMode::Replace) {
                    models = configured_models
                        .map(|configured| {
                            let mut enriched = configured;
                            if let Some(advertised) =
                                models.iter().find(|model| model.id == enriched.id)
                            {
                                enriched.clone_from(advertised);
                            }
                            enriched
                        })
                        .collect();
                    HarnessModelsSource::Config
                } else {
                    for configured in configured_models {
                        if !models.iter().any(|model| model.id == configured.id) {
                            models.push(configured);
                        }
                    }
                    HarnessModelsSource::Merged
                }
            } else {
                HarnessModelsSource::Discovered
            };
            offering.capabilities.models = Some(models);
            offering.capabilities.models_source = Some(source);
            offering.capabilities.turn_options = Some(true);
        }
        Ok(())
    }
}

pub const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);

const DISCOVERY_RETRY: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub(crate) struct Discoveries {
    advertised: Mutex<BTreeMap<HarnessId, Advertisement>>,
    commands: Mutex<BTreeMap<HarnessId, Vec<Command>>>,
    attempts: Mutex<BTreeMap<HarnessId, Instant>>,
}

impl Discoveries {
    pub(crate) fn advertised(&self, harness: HarnessId) -> Option<Advertisement> {
        self.advertised.lock().get(&harness).cloned()
    }

    pub(crate) fn remember(&self, harness: HarnessId, advertisement: Advertisement) {
        self.advertised.lock().insert(harness, advertisement);
    }

    pub(crate) fn record(&self, harness: HarnessId, advertisement: Advertisement) {
        self.commands
            .lock()
            .insert(harness, advertisement.commands().to_vec());
        self.remember(harness, advertisement);
    }

    pub(crate) fn start_due(self: &Arc<Self>, pending: Vec<(HarnessId, AcpCommand, PathBuf)>) {
        for (harness, command, cwd) in pending {
            {
                let mut attempts = self.attempts.lock();
                let due = attempts
                    .get(&harness)
                    .is_none_or(|at| at.elapsed() >= DISCOVERY_RETRY);
                if !due || self.advertised.lock().contains_key(&harness) {
                    continue;
                }
                attempts.insert(harness, Instant::now());
            }
            let discoveries = Arc::clone(self);
            tokio::spawn(async move {
                match discover(harness, &command, &cwd, DEFAULT_DISCOVERY_TIMEOUT).await {
                    Ok(found) => {
                        discoveries.record(harness, found);
                        discoveries.attempts.lock().remove(&harness);
                    }
                    Err(error) => {
                        tracing::warn!(%harness, %error, "harness discovery failed; offering it without chat");
                        discoveries.attempts.lock().insert(harness, Instant::now());
                    }
                }
            });
        }
    }
}

pub(crate) async fn discover(
    harness: HarnessId,
    command: &AcpCommand,
    cwd: &Path,
    timeout: Duration,
) -> Result<Advertisement, AcpError> {
    tokio::fs::create_dir_all(cwd)
        .await
        .map_err(|source| AcpError::DiscoveryDir {
            path: cwd.to_path_buf(),
            source,
        })?;
    let agent = tokio::time::timeout(timeout, crate::connection::start_new(command, cwd, None))
        .await
        .map_err(|_| AcpError::DiscoveryTimeout { timeout })??;
    let mut advertised = Advertisement::from_agent(&agent);
    let mut events = agent.events_rx;
    if let Some(deletion) = Deletion::offered(harness, &agent.capabilities) {
        let deleted = request(&agent.commands, |reply| SessionCommand::Delete {
            acp_session: agent.acp_session.clone(),
            deletion,
            reply,
        });
        let outcome = tokio::time::timeout(timeout, advertised.observing(&mut events, deleted))
            .await
            .map_err(|_| AcpError::DiscoveryTimeout { timeout })
            .and_then(|done| done.map_err(|failure| port_failure(failure, deletion.method())));
        if let Err(error) = outcome {
            tracing::debug!(%harness, %error, "harness discovery could not delete its session");
        }
    }
    agent.shutdown.cancel();
    agent
        .reaped
        .await
        .map_err(|_| AcpError::Closed {
            method: "discovery",
        })?
        .map_err(|source| AcpError::Launch {
            program: command.program.display().to_string(),
            source,
        })?;
    agent.tasks.wait().await;
    while let Ok(event) = events.try_recv() {
        advertised.observe(event);
    }
    Ok(advertised)
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Deletion {
    Standard,
    Grok,
}

impl Deletion {
    fn offered(harness: HarnessId, capabilities: &acp::AgentCapabilities) -> Option<Self> {
        if capabilities.session_capabilities.delete.is_some() {
            Some(Self::Standard)
        } else if harness == HarnessId::GrokBuild {
            Some(Self::Grok)
        } else {
            None
        }
    }

    const fn method(self) -> &'static str {
        match self {
            Self::Standard => "session/delete",
            Self::Grok => "_x.ai/session/delete",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcRequest)]
#[request(method = "_x.ai/session/delete", response = GrokDeleted)]
#[serde(rename_all = "camelCase")]
struct GrokDelete {
    session_id: acp::SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcResponse)]
struct GrokDeleted {
    success: bool,
}

pub(crate) async fn delete(
    connection: &ConnectionTo<Agent>,
    acp_session: acp::SessionId,
    deletion: Deletion,
) -> Result<(), acp::Error> {
    match deletion {
        Deletion::Standard => connection
            .send_request(acp::DeleteSessionRequest::new(acp_session))
            .block_task()
            .await
            .map(drop),
        Deletion::Grok => {
            let deleted = connection
                .send_request(GrokDelete {
                    session_id: acp_session,
                })
                .block_task()
                .await?;
            if deleted.success {
                Ok(())
            } else {
                Err(acp::Error::internal_error().data("grok did not delete the session"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Advertisement;
    use crate::options::SessionOptions;
    use crate::sheets::{SheetInput, offering};
    use agent_client_protocol::schema::v1 as acp;
    use config::ModelsMode;
    use kernel::harness::HarnessModelsSource;
    use std::sync::Arc;
    use wire::harness::HarnessId;

    #[test]
    fn configured_models_keep_advertised_labels_and_defaults() {
        let options = Arc::new(SessionOptions::default());
        options.replace(vec![
            acp::SessionConfigOption::select(
                "model",
                "Model",
                "opus",
                vec![
                    acp::SessionConfigSelectOption::new("opus", "Opus advertised"),
                    acp::SessionConfigSelectOption::new("fast", "Fast advertised"),
                ],
            )
            .category(acp::SessionConfigOptionCategory::Model),
        ]);
        let advertisement = Advertisement {
            capabilities: acp::AgentCapabilities::new(),
            options,
            commands: Vec::new(),
        };
        for (mode, source, ids) in [
            (
                ModelsMode::Replace,
                HarnessModelsSource::Config,
                vec!["opus", "custom/model"],
            ),
            (
                ModelsMode::Merge,
                HarnessModelsSource::Merged,
                vec!["opus", "fast", "custom/model"],
            ),
            (
                ModelsMode::Discover,
                HarnessModelsSource::Discovered,
                vec!["opus", "fast"],
            ),
        ] {
            let input = SheetInput {
                mode: Some(mode),
                models: Some(vec!["opus".to_owned(), "custom/model".to_owned()]),
                efforts: None,
            };
            let mut offering = offering(HarnessId::ClaudeCode, true, Some(&input)).expect("sheet");
            advertisement
                .apply(&mut offering, Some(&input))
                .expect("advertisement");
            assert_eq!(offering.capabilities.models_source, Some(source));
            let models = offering.capabilities.models.expect("models");
            assert_eq!(
                models
                    .iter()
                    .map(|model| model.id.as_str())
                    .collect::<Vec<_>>(),
                ids
            );
            let opus = models.first().expect("opus");
            assert_eq!(opus.label, "Opus advertised");
            assert_eq!(opus.is_default, Some(true));
        }
    }
}
