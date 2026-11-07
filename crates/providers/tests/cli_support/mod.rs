use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kernel::harness::{ApprovalDecision, HarnessOffering};
use kernel::harness::{
    HarnessAdapter, HarnessAdapterError, SessionEvent, SessionOption, SessionSpec, TurnInput,
};
use kernel::model::{ModelRequest, ModelStreamItem};
use parking_lot::Mutex;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use wire::session_id::SessionId;

pub(crate) type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    NoModeOption,
    New,
    Events,
    EventsClosed,
    Prompt,
    PromptAfterEvents,
    Cancel,
    Release,
    Permission,
    PermissionHang,
    Offerings,
}

#[derive(Debug)]
pub(crate) enum Action {
    New(SessionSpec, SessionId),
    Prompt(SessionId, TurnInput),
    Cancel(SessionId),
    Release(SessionId),
    Permission(ApprovalDecision),
}

pub(crate) struct Plan {
    pub(crate) events: Vec<SessionEvent>,
    pub(crate) finish: bool,
}

struct Session {
    spec: SessionSpec,
    mode: Option<String>,
    events: Option<mpsc::Receiver<SessionEvent>>,
    output: mpsc::Sender<SessionEvent>,
    permission: Arc<Notify>,
    cancelled: CancellationToken,
    plan: Option<Plan>,
}

pub(crate) struct Harness {
    plans: Mutex<VecDeque<Plan>>,
    sessions: Mutex<BTreeMap<SessionId, Session>>,
    actions: mpsc::UnboundedSender<Action>,
    pub(crate) fault: Option<Fault>,
    pub(crate) offerings: Vec<HarnessOffering>,
    pub(crate) started: Notify,
    pub(crate) start_gate: Option<Arc<Notify>>,
}

impl Harness {
    pub(crate) fn new(plans: Vec<Plan>) -> (Self, mpsc::UnboundedReceiver<Action>) {
        let (actions, receiver) = mpsc::unbounded_channel();
        (
            Self {
                plans: Mutex::new(plans.into()),
                sessions: Mutex::new(BTreeMap::new()),
                actions,
                fault: None,
                offerings: Vec::new(),
                started: Notify::new(),
                start_gate: None,
            },
            receiver,
        )
    }

    fn check(&self, point: Fault) -> Result<(), HarnessAdapterError> {
        if self.fault == Some(point) {
            return Err(HarnessAdapterError::Failed {
                message: format!("fixture {point:?} failure"),
                source: std::io::Error::other("fixture failure").into(),
            });
        }
        Ok(())
    }

    fn action(&self, action: Action) {
        self.actions
            .send(action)
            .expect("action receiver lives until shutdown");
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.sessions.lock().is_empty()
    }
}

#[async_trait]
impl HarnessAdapter for Harness {
    async fn new_session(&self, spec: SessionSpec) -> Result<SessionId, HarnessAdapterError> {
        self.started.notify_one();
        if let Some(gate) = &self.start_gate {
            gate.notified().await;
        }
        self.check(Fault::New)?;
        let session = SessionId::new(spec.harness, &format!("native-{}", uuid::Uuid::new_v4()));
        let (output, events) = mpsc::channel(2);
        let plan = self.plans.lock().pop_front().expect("one plan per turn");
        self.sessions.lock().insert(
            session.clone(),
            Session {
                spec: spec.clone(),
                mode: None,
                events: Some(events),
                output,
                permission: Arc::new(Notify::new()),
                cancelled: CancellationToken::new(),
                plan: Some(plan),
            },
        );
        self.action(Action::New(spec, session.clone()));
        Ok(session)
    }

    async fn prompt(
        &self,
        session: &SessionId,
        input: TurnInput,
    ) -> Result<(), HarnessAdapterError> {
        self.check(Fault::Prompt)?;
        let (output, permission, cancelled, plan) = {
            let mut sessions = self.sessions.lock();
            let entry = sessions
                .get_mut(session)
                .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?;
            let state = (
                entry.output.clone(),
                Arc::clone(&entry.permission),
                entry.cancelled.clone(),
                entry
                    .plan
                    .take()
                    .ok_or_else(|| HarnessAdapterError::TurnInFlight(session.clone()))?,
            );
            drop(sessions);
            state
        };
        self.action(Action::Prompt(session.clone(), input));
        for event in plan.events {
            let needs_permission = matches!(event, SessionEvent::Permission(_));
            output
                .send(event)
                .await
                .expect("provider reads prompt events");
            if needs_permission {
                permission.notified().await;
            }
        }
        if plan.finish {
            output
                .send(SessionEvent::TurnEnded)
                .await
                .expect("provider awaits turn end");
        } else {
            cancelled.cancelled().await;
        }
        self.check(Fault::PromptAfterEvents)?;
        Ok(())
    }

    async fn cancel(&self, session: &SessionId) -> Result<(), HarnessAdapterError> {
        self.action(Action::Cancel(session.clone()));
        self.check(Fault::Cancel)?;
        let sessions = self.sessions.lock();
        let entry = sessions
            .get(session)
            .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?;
        entry.cancelled.cancel();
        drop(sessions);
        Ok(())
    }

    async fn release(&self, session: &SessionId) -> Result<(), HarnessAdapterError> {
        let entry = self
            .sessions
            .lock()
            .remove(session)
            .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?;
        entry.cancelled.cancel();
        self.action(Action::Release(session.clone()));
        self.check(Fault::Release)
    }

    async fn answer_permission(
        &self,
        session: &SessionId,
        _: &str,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessAdapterError> {
        self.check(Fault::Permission)?;
        if self.fault == Some(Fault::PermissionHang) {
            return std::future::pending().await;
        }
        let sessions = self.sessions.lock();
        let entry = sessions
            .get(session)
            .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?;
        self.action(Action::Permission(decision));
        entry.permission.notify_one();
        drop(sessions);
        Ok(())
    }

    async fn set_option(
        &self,
        session: &SessionId,
        option: SessionOption,
    ) -> Result<(), HarnessAdapterError> {
        if matches!(self.fault, Some(Fault::NoModeOption))
            && matches!(option, SessionOption::Mode(_))
        {
            return Err(HarnessAdapterError::OptionUnavailable {
                option: "mode".into(),
            });
        }
        let mut sessions = self.sessions.lock();
        let entry = sessions
            .get_mut(session)
            .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?;
        match option {
            SessionOption::Mode(mode) => entry.mode = Some(mode),
            SessionOption::Model(model) => entry.spec.model = Some(model),
            SessionOption::Effort(effort) => entry.spec.effort = Some(effort),
        }
        drop(sessions);
        Ok(())
    }

    async fn events(
        &self,
        session: &SessionId,
    ) -> Result<mpsc::Receiver<SessionEvent>, HarnessAdapterError> {
        self.check(Fault::Events)?;
        if self.fault == Some(Fault::EventsClosed) {
            let (sender, receiver) = mpsc::channel(1);
            drop(sender);
            return Ok(receiver);
        }
        self.sessions
            .lock()
            .get_mut(session)
            .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?
            .events
            .take()
            .ok_or_else(|| HarnessAdapterError::EventStreamTaken(session.clone()))
    }

    async fn offerings(&self) -> Result<Vec<HarnessOffering>, HarnessAdapterError> {
        self.check(Fault::Offerings)?;
        Ok(self.offerings.clone())
    }
}

pub(crate) async fn action(actions: &mut mpsc::UnboundedReceiver<Action>) -> Action {
    tokio::time::timeout(Duration::from_secs(5), actions.recv())
        .await
        .expect("action timeout")
        .expect("action stream")
}

pub(crate) async fn items(mut receiver: mpsc::Receiver<ModelStreamItem>) -> Vec<ModelStreamItem> {
    tokio::time::timeout(Duration::from_secs(5), async move {
        let mut items = Vec::new();
        while let Some(item) = receiver.recv().await {
            items.push(item);
        }
        items
    })
    .await
    .expect("model stream timeout")
}

pub(crate) fn request() -> ModelRequest {
    ModelRequest {
        options: kernel::model::ModelOptions::default(),
        model: "test-model".into(),
        messages: vec![wire::message::Message {
            role: wire::message::Role::User,
            content: wire::message::MessageContent::Text("Say hi.".into()),
            reasoning: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        tools: Vec::new(),
        thinking: None,
        max_tokens: None,
    }
}

pub(crate) fn embedder(endpoint: String) -> providers::embed::Embedder {
    providers::embed::Embedder::build(providers::embed::EmbedConfig {
        endpoint,
        api_prefix: None,
        model: "embedding-model".into(),
        api_key: None,
        expected_dims: None,
        timeout_ms: Some(5000),
        query_instruction: None,
    })
    .expect("embedder")
}

pub(crate) fn provider(
    harness: Arc<Harness>,
    config: config::provider::CliProvider,
    cancel: &CancellationToken,
) -> providers::cli::CliBacked {
    providers::cli::CliBacked::build(
        config::provider::ProviderType::CodexCli,
        config,
        harness,
        embedder("http://127.0.0.1:1".into()),
        cancel,
    )
    .expect("CLI provider")
}
