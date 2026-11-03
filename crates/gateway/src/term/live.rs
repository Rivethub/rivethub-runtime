use std::path::PathBuf;

use strum::IntoEnumIterator as _;
use tokio::sync::broadcast;
use wire::harness::{
    HarnessEvent, HarnessId, HarnessSessionStatus, HarnessSessionSummary, RosterCommand,
    SessionDriver,
};

use kernel::harness::ToolCallUpdate;
use kernel::preview::{LiveActivity, Preview};
use kernel::terminal::PaneTurn;
use wire::plugin::process::PluginEvent;
use wire::session_id::SessionId;

use super::{LaunchCommand, TerminalService};

const HARNESS_EVENT_BUFFER: usize = 256;
pub(crate) const REGISTRY_EVENT_BUFFER: usize = 256;

#[derive(Debug, Clone)]
pub(crate) struct RegistryEvent {
    pub(crate) key: String,
    pub(crate) event: HarnessEvent,
}

fn serves(command: &LaunchCommand, harness: HarnessId) -> bool {
    command.harness == Some(harness) || command.id == RosterCommand::from(harness).to_string()
}

impl TerminalService {
    pub(crate) fn roster(&self) -> Vec<HarnessId> {
        HarnessId::iter()
            .filter(|harness| {
                self.options
                    .commands
                    .iter()
                    .any(|command| serves(command, *harness))
            })
            .collect()
    }

    pub(crate) fn roster_cwd(&self, harness: HarnessId) -> Option<PathBuf> {
        self.options
            .commands
            .iter()
            .find(|command| serves(command, harness))
            .map(|command| command.cwd.clone())
    }

    pub(crate) fn live_summary(&self, key: &str) -> Option<HarnessSessionSummary> {
        self.summaries.read().get(key).cloned()
    }

    pub(crate) fn chat_at_rest(&self, key: &str) -> bool {
        self.chats.read().contains_key(key)
            && !self.pty_running(key)
            && self.summaries.read().get(key).is_none_or(|summary| {
                summary.blocked != Some(true) && summary.status != HarnessSessionStatus::Active
            })
    }

    pub(crate) fn is_live(&self, key: &str) -> bool {
        let running = self
            .summaries
            .read()
            .get(key)
            .is_some_and(|summary| summary.status != HarnessSessionStatus::Ended);
        running || self.chats.read().contains_key(key) || self.pty_running(key)
    }

    pub(crate) fn pty_running(&self, key: &str) -> bool {
        let spawned = self.entries.read().get(key).is_some_and(|handle| {
            handle
                .state
                .borrow()
                .as_ref()
                .is_some_and(|state| state.state == wire::gateway::term::PtyState::Running)
        });
        spawned
            || self.adopted.read().values().any(|(owner, info)| {
                owner == key && info.state == wire::gateway::term::PtyState::Running
            })
    }

    pub(crate) fn live_summaries(
        &self,
        harness: HarnessId,
    ) -> Vec<(String, HarnessSessionSummary)> {
        self.summaries
            .read()
            .iter()
            .filter(|(_, summary)| summary.harness_id == harness)
            .map(|(key, summary)| (key.clone(), summary.clone()))
            .collect()
    }

    pub(crate) fn subscribe_harness(&self, key: &str) -> broadcast::Receiver<HarnessEvent> {
        if let Some(sender) = self.harness_events.read().get(key) {
            return sender.subscribe();
        }
        self.harness_events
            .write()
            .entry(key.to_owned())
            .or_insert_with(|| broadcast::channel(HARNESS_EVENT_BUFFER).0)
            .subscribe()
    }

    pub(crate) fn release_harness(&self, key: &str) {
        let mut senders = self.harness_events.write();
        if senders
            .get(key)
            .is_some_and(|sender| sender.receiver_count() == 0)
        {
            senders.remove(key);
        }
        drop(senders);
    }

    pub(crate) fn publish_harness(&self, key: &str, event: HarnessEvent) {
        self.follow(key, &event);
        self.hold_approvals(key, &event);
        if matches!(event, HarnessEvent::Context { .. }) {
            self.contexts.write().insert(key.to_owned(), event.clone());
        }
        if let HarnessEvent::Driver { driver, busy, .. } = &event {
            if *busy && *driver == Some(SessionDriver::Terminal) {
                self.contexts.write().remove(key);
            }
            self.drivers.write().insert(key.to_owned(), event.clone());
        }
        let sender = self.harness_events.read().get(key).cloned();
        if let Some(sender) = sender
            && sender.send(event).is_err()
        {
            tracing::trace!("harness event has no live session socket");
        }
    }

    pub(crate) fn pending_approvals(&self, key: &str) -> Vec<HarnessEvent> {
        self.approvals.read().get(key).cloned().unwrap_or_default()
    }

    pub(crate) fn held_context(&self, key: &str) -> Option<HarnessEvent> {
        self.contexts.read().get(key).cloned()
    }

    pub(crate) fn held_driver(&self, key: &str) -> Option<HarnessEvent> {
        self.drivers.read().get(key).cloned()
    }

    pub(crate) fn forget_driver(&self, key: &str) {
        self.drivers.write().remove(key);
    }

    fn hold_approvals(&self, key: &str, event: &HarnessEvent) {
        let mut approvals = self.approvals.write();
        match event {
            HarnessEvent::ApprovalRequest { .. } => {
                approvals
                    .entry(key.to_owned())
                    .or_default()
                    .push(event.clone());
            }
            HarnessEvent::ApprovalResolved { request_id, .. } => {
                if let Some(waiting) = approvals.get_mut(key) {
                    waiting.retain(|held| {
                        !matches!(held, HarnessEvent::ApprovalRequest { request_id: id, .. } if id == request_id)
                    });
                }
            }
            HarnessEvent::TurnComplete { .. } | HarnessEvent::Error { .. } => {
                approvals.remove(key);
            }
            HarnessEvent::SessionUpdated { status, .. }
                if *status != HarnessSessionStatus::Active =>
            {
                approvals.remove(key);
            }
            HarnessEvent::AssistantDelta { .. }
            | HarnessEvent::ReasoningDelta { .. }
            | HarnessEvent::ToolUse { .. }
            | HarnessEvent::ToolUpdate { .. }
            | HarnessEvent::ToolResult { .. }
            | HarnessEvent::SessionCreated { .. }
            | HarnessEvent::Plan { .. }
            | HarnessEvent::SessionUpdated { .. }
            | HarnessEvent::Context { .. }
            | HarnessEvent::Transcript { .. }
            | HarnessEvent::Status(_)
            | HarnessEvent::Driver { .. }
            | HarnessEvent::Prompt { .. } => {}
        }
    }

    pub(crate) fn tool_updated(&self, key: &str, update: &ToolCallUpdate) {
        if let Some(live) = self.activity.write().get_mut(key) {
            live.tool_updated(
                &update.tool_call_id,
                update.title.as_deref(),
                update.input.as_ref(),
            );
        }
    }

    pub(crate) fn running_tool(&self, call: &str) -> Option<SessionId> {
        let key = self
            .activity
            .read()
            .iter()
            .find(|(_, live)| live.running(call))
            .map(|(key, _)| key.clone())?;
        self.summaries
            .read()
            .get(&key)
            .map(|summary| summary.session_id.clone())
    }

    pub(crate) fn live_preview(&self, key: &str) -> Option<Preview> {
        self.activity
            .read()
            .get(key)
            .and_then(LiveActivity::preview)
    }

    pub(crate) fn attach_plugins(&self, host: std::sync::Arc<crate::plugins::PluginHost>) {
        if self.plugins.set(host).is_err() {
            tracing::debug!("plug-in host already attached to the terminal service");
        }
    }

    fn tell_plugins(&self, event: &HarnessEvent) {
        let Some(host) = self.plugins.get() else {
            return;
        };
        let lifecycle = match event {
            HarnessEvent::TurnComplete { session_id, .. } => PluginEvent::SessionIdle {
                session_id: session_id.clone(),
            },
            HarnessEvent::SessionUpdated {
                session_id,
                status: HarnessSessionStatus::Ended,
                ..
            } => PluginEvent::SessionEnded {
                session_id: session_id.clone(),
            },
            HarnessEvent::SessionUpdated { .. }
            | HarnessEvent::AssistantDelta { .. }
            | HarnessEvent::ReasoningDelta { .. }
            | HarnessEvent::ToolUse { .. }
            | HarnessEvent::ToolUpdate { .. }
            | HarnessEvent::ToolResult { .. }
            | HarnessEvent::ApprovalRequest { .. }
            | HarnessEvent::ApprovalResolved { .. }
            | HarnessEvent::SessionCreated { .. }
            | HarnessEvent::Plan { .. }
            | HarnessEvent::Error { .. }
            | HarnessEvent::Context { .. }
            | HarnessEvent::Transcript { .. }
            | HarnessEvent::Status(_)
            | HarnessEvent::Driver { .. }
            | HarnessEvent::Prompt { .. } => return,
        };
        host.publish(&lifecycle);
    }

    fn follow(&self, key: &str, event: &HarnessEvent) {
        self.tell_plugins(event);
        let now = chrono::Utc::now();
        let mut activity = self.activity.write();
        match event {
            HarnessEvent::AssistantDelta { text, .. } => {
                activity.entry(key.to_owned()).or_default().wrote(text, now);
            }
            HarnessEvent::ToolUse {
                tool_call_id,
                name,
                input,
                ..
            } => activity.entry(key.to_owned()).or_default().tool_started(
                tool_call_id,
                name,
                input,
                now,
            ),
            HarnessEvent::ApprovalRequest {
                request_id,
                name,
                input,
                ..
            } => activity
                .entry(key.to_owned())
                .or_default()
                .approval_requested(request_id, name, input, now),
            HarnessEvent::ToolResult { tool_call_id, .. } => {
                if let Some(live) = activity.get_mut(key) {
                    live.tool_finished(tool_call_id);
                }
            }
            HarnessEvent::ApprovalResolved { request_id, .. } => {
                if let Some(live) = activity.get_mut(key) {
                    live.approval_resolved(request_id);
                }
            }
            HarnessEvent::TurnComplete { .. } | HarnessEvent::Error { .. } => {
                activity.remove(key);
            }
            HarnessEvent::SessionUpdated { status, .. }
                if *status != HarnessSessionStatus::Active =>
            {
                activity.remove(key);
            }
            HarnessEvent::SessionUpdated { .. }
            | HarnessEvent::ReasoningDelta { .. }
            | HarnessEvent::ToolUpdate { .. }
            | HarnessEvent::SessionCreated { .. }
            | HarnessEvent::Plan { .. }
            | HarnessEvent::Context { .. }
            | HarnessEvent::Transcript { .. }
            | HarnessEvent::Status(_)
            | HarnessEvent::Driver { .. }
            | HarnessEvent::Prompt { .. } => {}
        }
    }

    pub(crate) fn subscribe_registry(&self) -> broadcast::Receiver<RegistryEvent> {
        self.registry_events.subscribe()
    }

    pub(crate) fn tell_started(&self, session: &SessionId, topic: Option<String>) {
        if let Some(host) = self.plugins.get() {
            host.publish(&PluginEvent::SessionStarted {
                session_id: session.clone(),
                topic_id: topic,
            });
        }
    }

    pub(crate) fn publish_registry(&self, key: String, event: HarnessEvent) {
        if self
            .registry_events
            .send(RegistryEvent { key, event })
            .is_err()
        {
            tracing::trace!("registry event has no live harness socket");
        }
    }

    pub(crate) fn set_live_status(
        &self,
        key: &str,
        session: &SessionId,
        status: HarnessSessionStatus,
    ) {
        let settled = status != HarnessSessionStatus::Active;
        let blocked = match self.summaries.write().get_mut(key) {
            Some(summary) => {
                summary.status = status;
                summary.updated_at = chrono::Utc::now();
                if settled {
                    summary.blocked = None;
                }
                summary.blocked
            }
            None => None,
        };
        let reported = if settled { Some(false) } else { blocked };
        self.publish_update(key, session, status, reported);
    }

    pub(crate) fn chose(&self, key: &str, option: kernel::harness::SessionOption) {
        if let Some(summary) = self.summaries.write().get_mut(key) {
            match option {
                kernel::harness::SessionOption::Model(model) => summary.model = Some(model),
                kernel::harness::SessionOption::Effort(level) => {
                    summary.effort = Some(level.to_string());
                }
                kernel::harness::SessionOption::Mode(_) => {}
            }
        }
    }

    pub(crate) fn pane_changed(&self, key: &str, session: &SessionId, change: PaneTurn) {
        let live = self
            .summaries
            .read()
            .get(key)
            .map(|summary| summary.session_id.clone());
        match (live, change) {
            (Some(live), PaneTurn::Waiting(waiting)) => self.set_blocked(key, &live, waiting),
            (Some(_), PaneTurn::Ended) => {}
            (None, PaneTurn::Waiting(waiting)) => {
                self.publish_update(key, session, HarnessSessionStatus::Active, Some(waiting));
            }
            (None, PaneTurn::Ended) => {
                self.publish_update(key, session, HarnessSessionStatus::Idle, Some(false));
            }
        }
    }

    pub(crate) fn set_blocked(&self, key: &str, session: &SessionId, blocked: bool) {
        let status = match self.summaries.write().get_mut(key) {
            Some(summary) if summary.blocked.unwrap_or(false) != blocked => {
                summary.blocked = blocked.then_some(true);
                summary.status
            }
            _ => return,
        };
        self.publish_update(key, session, status, Some(blocked));
    }

    fn publish_update(
        &self,
        key: &str,
        session: &SessionId,
        status: HarnessSessionStatus,
        blocked: Option<bool>,
    ) {
        let event = HarnessEvent::SessionUpdated {
            session_id: session.clone(),
            status,
            previous_session_id: None,
            supersedes: None,
            blocked,
        };
        self.publish_harness(key, event.clone());
        self.publish_registry(key.to_owned(), event);
    }
}
