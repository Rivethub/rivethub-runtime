use std::collections::BTreeMap;

use async_trait::async_trait;
use kernel::harness::{ApprovalDecision, HarnessOffering};
use kernel::harness::{
    HarnessAdapter, HarnessAdapterError, SessionEvent, SessionOption, SessionSpec, TurnInput,
};
use tokio::sync::{Mutex, mpsc};
use wire::session_id::SessionId;

struct Session {
    output: mpsc::Sender<SessionEvent>,
    events: Option<mpsc::Receiver<SessionEvent>>,
    options: Vec<SessionOption>,
}

#[derive(Default)]
pub(crate) struct Harness {
    sessions: Mutex<BTreeMap<SessionId, Session>>,
    pub(crate) requests: Mutex<Vec<SessionSpec>>,
    pub(crate) prompts: Mutex<Vec<TurnInput>>,
    pub(crate) options: Mutex<Vec<SessionOption>>,
    pub(crate) events: Vec<SessionEvent>,
}

impl Harness {
    pub(crate) fn new(events: Vec<SessionEvent>) -> Self {
        Self {
            events,
            ..Self::default()
        }
    }
}

#[async_trait]
impl HarnessAdapter for Harness {
    async fn new_session(&self, spec: SessionSpec) -> Result<SessionId, HarnessAdapterError> {
        let (output, events) = mpsc::channel(16);
        let session = spec.session.clone();
        self.requests.lock().await.push(spec);
        self.sessions.lock().await.insert(
            session.clone(),
            Session {
                output,
                events: Some(events),
                options: Vec::new(),
            },
        );
        Ok(session)
    }

    async fn prompt(
        &self,
        session: &SessionId,
        input: TurnInput,
    ) -> Result<(), HarnessAdapterError> {
        let output = self
            .sessions
            .lock()
            .await
            .get(session)
            .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?
            .output
            .clone();
        self.prompts.lock().await.push(input);
        for event in &self.events {
            output
                .send(event.clone())
                .await
                .map_err(|source| HarnessAdapterError::Failed {
                    message: "test event receiver closed".into(),
                    source: Box::new(source),
                })?;
        }
        Ok(())
    }

    async fn cancel(&self, session: &SessionId) -> Result<(), HarnessAdapterError> {
        self.known(session).await
    }

    async fn release(&self, session: &SessionId) -> Result<(), HarnessAdapterError> {
        let removed = self
            .sessions
            .lock()
            .await
            .remove(session)
            .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?;
        self.options.lock().await.extend(removed.options);
        Ok(())
    }

    async fn answer_permission(
        &self,
        session: &SessionId,
        _: &str,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessAdapterError> {
        assert_eq!(decision, ApprovalDecision::Deny);
        self.known(session).await
    }

    async fn set_option(
        &self,
        session: &SessionId,
        option: SessionOption,
    ) -> Result<(), HarnessAdapterError> {
        self.sessions
            .lock()
            .await
            .get_mut(session)
            .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?
            .options
            .push(option);
        Ok(())
    }

    async fn events(
        &self,
        session: &SessionId,
    ) -> Result<mpsc::Receiver<SessionEvent>, HarnessAdapterError> {
        self.sessions
            .lock()
            .await
            .get_mut(session)
            .ok_or_else(|| HarnessAdapterError::UnknownSession(session.clone()))?
            .events
            .take()
            .ok_or_else(|| HarnessAdapterError::EventStreamTaken(session.clone()))
    }

    async fn offerings(&self) -> Result<Vec<HarnessOffering>, HarnessAdapterError> {
        Ok(Vec::new())
    }
}

impl Harness {
    async fn known(&self, session: &SessionId) -> Result<(), HarnessAdapterError> {
        if self.sessions.lock().await.contains_key(session) {
            Ok(())
        } else {
            Err(HarnessAdapterError::UnknownSession(session.clone()))
        }
    }
}
