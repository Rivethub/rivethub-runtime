use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::JsonObject;
use crate::gateway::memory::SummaryKind;
use crate::gateway::topics::PlacedBy;
use crate::plugin::action::ViewAction;
use crate::plugin::host::{PluginJobOutcome, PluginJobTrigger};
use crate::plugin::manifest::Permission;
use crate::plugin::view::{ViewTone, ViewValue};
use crate::plugin::{PluginId, RouteName};
use crate::session_id::SessionId;

#[serde_as]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginEventDelivery {
    pub seq: u64,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub at: DateTime<Utc>,
    pub missed: u64,
    pub event: PluginEvent,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum PluginEvent {
    #[serde(rename = "session.started", rename_all = "camelCase")]
    SessionStarted {
        session_id: SessionId,
        topic_id: Option<String>,
    },
    #[serde(rename = "session.idle", rename_all = "camelCase")]
    SessionIdle { session_id: SessionId },
    #[serde(rename = "session.ended", rename_all = "camelCase")]
    SessionEnded { session_id: SessionId },
    #[serde(rename = "session.placed", rename_all = "camelCase")]
    SessionPlaced {
        session_id: SessionId,
        topic_id: String,
        by: PlacedBy,
    },
    #[serde(rename = "summary.written", rename_all = "camelCase")]
    SummaryWritten {
        summary_id: String,
        session_id: Option<SessionId>,
        kind: SummaryKind,
        level: u32,
    },
    #[serde(rename = "settings.changed", rename_all = "camelCase")]
    SettingsChanged { values: BTreeMap<String, ViewValue> },
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginJobRequest {
    pub run_id: String,
    pub trigger: PluginJobTrigger,
    #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
    pub scheduled_at: Option<DateTime<Utc>>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginJobResult {
    pub outcome: PluginJobOutcome,
    pub message: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginToolCall {
    pub arguments: JsonObject,
    pub session_id: Option<SessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginToolResult {
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginSelf {
    pub id: PluginId,
    pub node: String,
    pub version: String,
    pub permissions: Vec<Permission>,
    pub settings: BTreeMap<String, ViewValue>,
    pub data_dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginViewsChanged {
    pub views: Vec<RouteName>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginNotify {
    pub title: String,
    pub text: Option<String>,
    pub tone: Option<ViewTone>,
    pub action: Option<ViewAction>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename = "plugin.notification", rename_all = "camelCase")]
pub struct PluginNotification {
    pub plugin: PluginId,
    pub node: String,
    pub title: String,
    pub text: Option<String>,
    pub tone: Option<ViewTone>,
    pub action: Option<ViewAction>,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub ts: DateTime<Utc>,
}
