mod catalog;
mod deletion;
mod provider;
mod reader;
pub use catalog::FakeSessionCatalog;
pub use deletion::FakeSessionDeletion;
pub use provider::{FakeProvider, Script};
pub use reader::FakeReader;
mod queue;
pub use queue::FakeJobQueue;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use kernel::ToolContext;
use kernel::harness::ApprovalDecision;
use kernel::harness::HarnessAdapter;
use kernel::harness::HarnessAdapterError;
use kernel::harness::HarnessCommands;
use kernel::harness::SessionEvent;
use kernel::harness::SessionOption;
use kernel::harness::SessionSpec;
use kernel::harness::SystemPromptSupport;
use kernel::harness::TurnInput;
use kernel::terminal::PtyInfo;
use kernel::terminal::TermSize;
use kernel::terminal::TerminalId;
use kernel::terminal::TerminalMux;
use kernel::terminal::TerminalMuxError;
use kernel::tool::Tool;
use kernel::tool::ToolError;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Receiver;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

#[derive(Debug)]
struct SessionStream {
    tx: tokio::sync::mpsc::Sender<SessionEvent>,
    rx: Option<Receiver<SessionEvent>>,
}

#[derive(Debug, Default)]
pub struct FakeHarness {
    queued: Mutex<Vec<SessionEvent>>,
    sessions: Mutex<BTreeMap<SessionId, SessionStream>>,
    offerings: parking_lot::Mutex<Vec<kernel::harness::HarnessOffering>>,
    commands: parking_lot::Mutex<Vec<HarnessCommands>>,
}

impl FakeHarness {
    pub fn new(events: Vec<SessionEvent>) -> Self {
        Self {
            queued: Mutex::new(events),
            sessions: Mutex::new(BTreeMap::new()),
            offerings: parking_lot::Mutex::new(Vec::new()),
            commands: parking_lot::Mutex::new(Vec::new()),
        }
    }

    pub fn set_offerings(&self, offerings: Vec<kernel::harness::HarnessOffering>) {
        *self.offerings.lock() = offerings;
    }

    pub fn set_commands(&self, commands: Vec<HarnessCommands>) {
        *self.commands.lock() = commands;
    }
}

#[async_trait]
impl HarnessAdapter for FakeHarness {
    fn system_prompt(&self, _harness: HarnessId) -> SystemPromptSupport {
        SystemPromptSupport::MetaAppend
    }

    async fn new_session(&self, spec: SessionSpec) -> Result<SessionId, HarnessAdapterError> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        self.sessions
            .lock()
            .await
            .insert(spec.session.clone(), SessionStream { tx, rx: Some(rx) });
        Ok(spec.session)
    }

    async fn resume(
        &self,
        session: &SessionId,
        _cwd: Option<&std::path::Path>,
    ) -> Result<(), HarnessAdapterError> {
        let mut sessions = self.sessions.lock().await;
        if !sessions.contains_key(session) {
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            sessions.insert(session.clone(), SessionStream { tx, rx: Some(rx) });
        }
        drop(sessions);
        Ok(())
    }

    async fn prompt(
        &self,
        session: &SessionId,
        _input: TurnInput,
    ) -> Result<(), HarnessAdapterError> {
        let events: Vec<SessionEvent> = self.queued.lock().await.drain(..).collect();
        let tx = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(session)
                .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?
                .tx
                .clone()
        };
        for event in events {
            tx.send(event)
                .await
                .map_err(|source| HarnessAdapterError::Failed {
                    message: "scripted event receiver closed".to_owned(),
                    source: Box::new(source),
                })?;
        }
        Ok(())
    }

    async fn cancel(&self, _session: &SessionId) -> Result<(), HarnessAdapterError> {
        Ok(())
    }

    async fn answer_permission(
        &self,
        _session: &SessionId,
        _request_id: &str,
        _decision: ApprovalDecision,
    ) -> Result<(), HarnessAdapterError> {
        Ok(())
    }

    async fn set_option(
        &self,
        _session: &SessionId,
        _option: SessionOption,
    ) -> Result<(), HarnessAdapterError> {
        Ok(())
    }

    async fn events(
        &self,
        session: &SessionId,
    ) -> Result<Receiver<SessionEvent>, HarnessAdapterError> {
        let mut sessions = self.sessions.lock().await;
        let stream = sessions
            .get_mut(session)
            .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?;
        let taken = stream.rx.take();
        drop(sessions);
        taken.ok_or_else(|| HarnessAdapterError::EventStreamTaken(session.clone()))
    }

    async fn offerings(
        &self,
    ) -> Result<Vec<kernel::harness::HarnessOffering>, HarnessAdapterError> {
        Ok(self.offerings.lock().clone())
    }

    async fn commands(&self) -> Result<Vec<HarnessCommands>, HarnessAdapterError> {
        Ok(self.commands.lock().clone())
    }
}

#[derive(Debug, Default)]
pub struct FakeMux {
    ptys: Mutex<Vec<(TerminalId, SessionId, bool)>>,
    next_id: AtomicI64,
}

#[async_trait]
impl TerminalMux for FakeMux {
    async fn spawn(
        &self,
        session: &SessionId,
        _argv: Vec<String>,
        _cwd: &std::path::Path,
        _env: &BTreeMap<String, String>,
        _size: TermSize,
    ) -> Result<TerminalId, TerminalMuxError> {
        let id = TerminalId::new(format!(
            "pty-{}",
            self.next_id.fetch_add(1, Ordering::Relaxed)
        ));
        self.ptys
            .lock()
            .await
            .push((id.clone(), session.clone(), true));
        Ok(id)
    }

    async fn attach(&self, id: &TerminalId) -> Result<Receiver<Vec<u8>>, TerminalMuxError> {
        self.ptys
            .lock()
            .await
            .iter()
            .find(|(pty, _, _)| pty == id)
            .ok_or_else(|| TerminalMuxError::UnknownTerminal(id.clone()))?;
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        Ok(rx)
    }

    async fn input(&self, id: &TerminalId, _bytes: Vec<u8>) -> Result<(), TerminalMuxError> {
        self.ptys
            .lock()
            .await
            .iter()
            .find(|(pty, _, _)| pty == id)
            .ok_or_else(|| TerminalMuxError::UnknownTerminal(id.clone()))?;
        Ok(())
    }

    async fn resize(&self, id: &TerminalId, _size: TermSize) -> Result<(), TerminalMuxError> {
        self.ptys
            .lock()
            .await
            .iter()
            .find(|(pty, _, _)| pty == id)
            .ok_or_else(|| TerminalMuxError::UnknownTerminal(id.clone()))?;
        Ok(())
    }

    async fn kill(&self, id: &TerminalId) -> Result<(), TerminalMuxError> {
        let killed = {
            let mut ptys = self.ptys.lock().await;
            match ptys.iter_mut().find(|(pty, _, _)| pty == id) {
                Some(entry) => {
                    entry.2 = false;
                    true
                }
                None => false,
            }
        };
        if killed {
            Ok(())
        } else {
            Err(TerminalMuxError::UnknownTerminal(id.clone()))
        }
    }

    async fn list(&self) -> Result<Vec<PtyInfo>, TerminalMuxError> {
        Ok(self
            .ptys
            .lock()
            .await
            .iter()
            .map(|(id, session, running)| PtyInfo {
                id: id.clone(),
                session: session.clone(),
                running: *running,
            })
            .collect())
    }
}

#[derive(Debug)]
pub struct FakeTool {
    definition: rmcp::model::Tool,
}

impl FakeTool {
    pub fn new(name: &str) -> Result<Self, serde_json::Error> {
        let schema = schemars::schema_for!(EchoArguments);
        let definition = rmcp::model::Tool::new(
            name.to_owned(),
            "echo the text argument back",
            serde_json::from_value::<rmcp::model::JsonObject>(serde_json::to_value(schema)?)?,
        );
        Ok(Self { definition })
    }
}

#[async_trait]
impl Tool for FakeTool {
    fn definition(&self) -> rmcp::model::Tool {
        self.definition.clone()
    }

    async fn call(
        &self,
        arguments: Option<rmcp::model::JsonObject>,
        _ctx: ToolContext,
    ) -> Result<rmcp::model::CallToolResult, ToolError> {
        let arguments =
            arguments.ok_or_else(|| ToolError::InvalidArguments("text is required".to_owned()))?;
        let EchoArguments { text } =
            serde_json::from_value(serde_json::Value::Object(arguments))
                .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        Ok(rmcp::model::CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(text),
        ]))
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct EchoArguments {
    text: String,
}
