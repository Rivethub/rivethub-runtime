use std::sync::Arc;
use std::time::Duration;

use kernel::harness::ApprovalDecision;
use kernel::harness::{
    HarnessAdapter, HarnessAdapterError, SessionEvent, SessionOption, SessionSpec, TurnInput,
};
use kernel::model::{ModelChunk, ModelStreamItem};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use wire::session_id::SessionId;

use super::CliError;

pub(super) struct Turn {
    pub harness: Arc<dyn HarnessAdapter>,
    pub spec: SessionSpec,
    pub input: TurnInput,
    pub timeout: Option<Duration>,
    pub permission_mode: Option<String>,
    pub permission_timeout: Option<Duration>,
    pub cancel: CancellationToken,
    pub output: Sender<ModelStreamItem>,
}

impl Turn {
    fn permission_decision(&self) -> ApprovalDecision {
        if self.permission_mode.as_deref() == Some(BYPASS_PERMISSIONS) {
            ApprovalDecision::Allow
        } else {
            ApprovalDecision::Deny
        }
    }

    pub(super) async fn run(self) {
        let result = self.execute().await;
        if let Err(error) = &result {
            tracing::debug!(%error, "headless turn failed");
        }
        let item = result.map(|()| ModelChunk::Done).map_err(Into::into);
        tokio::select! {
            biased;
            result = self.output.send(item) => {
                if let Err(error) = result {
                    tracing::debug!(%error, "headless turn consumer disconnected");
                }
            }
            () = self.cancel.cancelled() => {
                tracing::debug!("headless turn stopped with a full output buffer");
            }
        }
    }

    async fn execute(&self) -> Result<(), CliError> {
        if self.cancel.is_cancelled() {
            return Err(CliError::Cancelled);
        }
        let session = self.harness.new_session(self.spec.clone()).await?;
        let result = tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(CliError::Cancelled),
            () = self.output.closed() => Err(CliError::Disconnected),
            () = deadline(self.timeout) => Err(CliError::Timeout),
            result = self.exchange(&session) => result,
        };
        let cancellation = if result.is_err() {
            self.harness.cancel(&session).await.map_err(CliError::from)
        } else {
            Ok(())
        };
        let release = self.harness.release(&session).await.map_err(CliError::from);
        combine(combine(result, cancellation), release)
    }

    async fn exchange(&self, session: &SessionId) -> Result<(), CliError> {
        if let Some(mode) = &self.permission_mode {
            match self
                .harness
                .set_option(session, SessionOption::Mode(mode.clone()))
                .await
            {
                Ok(()) => {}
                Err(HarnessAdapterError::OptionUnavailable { .. }) => {
                    tracing::debug!(%mode, "agent has no mode option; permission_mode applies client-side");
                }
                Err(error) => return Err(error.into()),
            }
        }
        let mut events = self.harness.events(session).await?;
        let prompt = self.harness.prompt(session, self.input.clone());
        tokio::pin!(prompt);
        let mut prompt_finished = false;
        let mut turn_ended = false;
        while !prompt_finished || !turn_ended {
            tokio::select! {
                result = &mut prompt, if !prompt_finished => {
                    result?;
                    prompt_finished = true;
                }
                event = next_event(&mut events), if !turn_ended => {
                    turn_ended = self.emit(session, event?).await?;
                }
            }
        }
        Ok(())
    }

    async fn emit(&self, session: &SessionId, event: SessionEvent) -> Result<bool, CliError> {
        let chunk = match event {
            SessionEvent::AgentMessageChunk { text } => ModelChunk::Text { delta: text },
            SessionEvent::AgentThoughtChunk { text } => ModelChunk::Reasoning { delta: text },
            SessionEvent::Usage { usage } => ModelChunk::Usage { usage },
            SessionEvent::Exited { code } => return Err(CliError::Exited { code }),
            SessionEvent::TurnEnded => return Ok(true),
            SessionEvent::Permission(request) => {
                tokio::select! {
                    result = self.harness.answer_permission(session, &request.request_id, self.permission_decision()) => result?,
                    () = deadline(self.permission_timeout) => return Err(CliError::Timeout),
                }
                return Ok(false);
            }
            SessionEvent::UserMessageChunk { .. }
            | SessionEvent::ToolCall(_)
            | SessionEvent::Context(_)
            | SessionEvent::ToolCallUpdate(_)
            | SessionEvent::Plan(_)
            | SessionEvent::AvailableCommands(_)
            | SessionEvent::CurrentMode { .. } => return Ok(false),
        };
        self.send(chunk).await?;
        Ok(false)
    }

    async fn send(&self, chunk: ModelChunk) -> Result<(), CliError> {
        self.output
            .send(Ok(chunk))
            .await
            .map_err(|_| CliError::Disconnected)
    }
}

async fn next_event(events: &mut Receiver<SessionEvent>) -> Result<SessionEvent, CliError> {
    events.recv().await.ok_or(CliError::Incomplete)
}

async fn deadline(timeout: Option<Duration>) {
    match timeout {
        Some(timeout) => tokio::time::sleep(timeout).await,
        None => std::future::pending().await,
    }
}

fn combine(result: Result<(), CliError>, cleanup: Result<(), CliError>) -> Result<(), CliError> {
    match (result, cleanup) {
        (Ok(()), result) | (result, Ok(())) => result,
        (Err(turn), Err(cleanup)) => Err(CliError::Cleanup {
            turn: Box::new(turn),
            cleanup: Box::new(cleanup),
        }),
    }
}

const BYPASS_PERMISSIONS: &str = "bypassPermissions";
