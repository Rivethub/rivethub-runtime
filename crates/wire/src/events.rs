use crate::{Extra, JsonObject, harness::SessionHarness};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(try_from = "u8", into = "u8")]
pub enum ProtocolVersion {
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("protocol version must be 1")]
pub struct ProtocolVersionError;

impl TryFrom<u8> for ProtocolVersion {
    type Error = ProtocolVersionError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value == 1 {
            Ok(Self::V1)
        } else {
            Err(ProtocolVersionError)
        }
    }
}

impl From<ProtocolVersion> for u8 {
    fn from(_value: ProtocolVersion) -> Self {
        1
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentEvent {
    pub v: ProtocolVersion,
    pub session: String,
    #[serde(flatten)]
    pub kind: AgentEventKind,
    #[serde(flatten, default)]
    pub extra: Extra,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum AgentEventKind {
    #[serde(rename = "session.start", rename_all = "camelCase")]
    SessionStart {
        title: Option<String>,
        name: Option<String>,
        harness: Option<SessionHarness>,
        #[serde(default)]
        #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
        ts: Option<DateTime<Utc>>,
    },
    #[serde(rename = "session.end")]
    SessionEnd {
        #[serde(default)]
        #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
        ts: Option<DateTime<Utc>>,
    },
    #[serde(rename = "message.user")]
    MessageUser {
        text: String,
        #[serde(default)]
        #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
        ts: Option<DateTime<Utc>>,
    },
    #[serde(rename = "message.agent", rename_all = "camelCase")]
    MessageAgent {
        text: String,
        model: Option<String>,
        duration_ms: Option<u64>,
        usage: Option<AgentUsage>,
        #[serde(default)]
        #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
        ts: Option<DateTime<Utc>>,
    },
    #[serde(rename = "thinking.delta")]
    ThinkingDelta {
        text: String,
        #[serde(default)]
        #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
        ts: Option<DateTime<Utc>>,
    },
    #[serde(rename = "tool.start", rename_all = "camelCase")]
    ToolStart {
        tool: String,
        args: Option<JsonObject>,
        #[serde(default)]
        #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
        ts: Option<DateTime<Utc>>,
    },
    #[serde(rename = "task.plan")]
    TaskPlan {
        tasks: Vec<String>,
        #[serde(default)]
        #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
        ts: Option<DateTime<Utc>>,
    },
    #[serde(rename = "task.check")]
    TaskCheck {
        index: u32,
        #[serde(default)]
        #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
        ts: Option<DateTime<Utc>>,
    },
    #[serde(rename = "activity")]
    Activity {
        activity: Activity,
        #[serde(default)]
        #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
        ts: Option<DateTime<Utc>>,
    },
    #[serde(rename = "approval.wait", rename_all = "camelCase")]
    ApprovalWait {
        tool: String,
        args: Option<JsonObject>,
        #[serde(default)]
        #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
        ts: Option<DateTime<Utc>>,
    },
    #[serde(rename = "turn.end")]
    TurnEnd {
        #[serde(default)]
        #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
        ts: Option<DateTime<Utc>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Idle,
    Thinking,
    Writing,
    Speaking,
    EditingCode,
    Sleeping,
    Computer,
    Toolbox,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ContextSource {
    Spawn,
    Default,
    Observed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ContextUsageSource {
    Harness,
    LastCall,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename = "LiveSessionSummary")]
pub struct SessionSummary {
    pub id: String,
    pub name: String,
    pub harness: Option<SessionHarness>,
    #[serde(default)]
    #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
    pub last_event_ts: Option<DateTime<Utc>>,
    pub pty: Option<String>,
    pub context_window: Option<u64>,
    pub compact_at: Option<u64>,
    pub context_source: Option<ContextSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionLogLine {
    pub who: String,
    pub text: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub title: String,
    pub activity: Activity,
    pub tool: Option<String>,
    pub tasks: Vec<String>,
    pub thought: String,
    pub last_message: String,
    pub log: Vec<SessionLogLine>,
    pub term: Vec<String>,
    pub ended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename = "LiveSessionList")]
pub struct SessionList {
    pub sessions: Vec<SessionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StateResponse {
    pub session: String,
    pub state: SessionView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename = "snapshot", rename_all = "camelCase")]
pub struct Snapshot {
    pub v: ProtocolVersion,
    pub sessions: Vec<SessionSummary>,
    #[serde(rename = "rooms")]
    pub views: BTreeMap<String, SessionView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename = "session.removed", rename_all = "camelCase")]
pub struct SessionRemoved {
    pub v: ProtocolVersion,
    pub session: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged, try_from = "crate::JsonObject")]
pub enum SessionBroadcast {
    Snapshot(Snapshot),
    Removed(SessionRemoved),
    HubState(crate::gateway::hub_state::HubStateChanged),
    PluginChanged(crate::plugin::host::PluginChangedNotice),
    PluginViews(crate::plugin::host::PluginViewsNotice),
    TopicChanged(crate::gateway::topics::TopicChangedNotice),
    Event(AgentEvent),
}

#[derive(Deserialize)]
enum BroadcastKind {
    #[serde(rename = "snapshot")]
    Snapshot,
    #[serde(rename = "session.removed")]
    Removed,
    #[serde(rename = "hub.state")]
    HubState,
    #[serde(rename = "plugin.changed")]
    PluginChanged,
    #[serde(rename = "plugin.views")]
    PluginViews,
    #[serde(rename = "topic.changed")]
    TopicChanged,
    #[serde(other)]
    Event,
}

#[derive(Deserialize)]
struct BroadcastTag {
    #[serde(rename = "type")]
    pub kind: BroadcastKind,
}

impl TryFrom<crate::JsonObject> for SessionBroadcast {
    type Error = serde_json::Error;

    fn try_from(frame: crate::JsonObject) -> Result<Self, Self::Error> {
        let BroadcastTag { kind } = frame.deserialize()?;
        match kind {
            BroadcastKind::Snapshot => frame.deserialize().map(Self::Snapshot),
            BroadcastKind::Removed => frame.deserialize().map(Self::Removed),
            BroadcastKind::HubState => frame.deserialize().map(Self::HubState),
            BroadcastKind::PluginChanged => frame.deserialize().map(Self::PluginChanged),
            BroadcastKind::PluginViews => frame.deserialize().map(Self::PluginViews),
            BroadcastKind::TopicChanged => frame.deserialize().map(Self::TopicChanged),
            BroadcastKind::Event => frame.deserialize().map(Self::Event),
        }
    }
}
