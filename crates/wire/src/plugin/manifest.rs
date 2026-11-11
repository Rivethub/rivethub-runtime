use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::JsonObject;
use crate::events::ProtocolVersion;
use crate::plugin::action::ViewAction;
use crate::plugin::view::ViewField;
use crate::plugin::{PluginId, RouteName, ToolName};

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginManifest {
    #[serde(rename = "$schema")]
    pub schema: Option<String>,
    pub manifest: ProtocolVersion,
    pub id: PluginId,
    pub name: String,
    pub version: String,
    pub description: String,
    pub icon: Option<String>,
    pub run: PluginRun,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<Permission>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<ViewField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<PluginTool>,
    #[serde(default)]
    pub contributes: PluginContributions,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<PluginJob>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<PluginEventKind>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginRun {
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub ready_timeout_ms: Option<u64>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum::Display,
)]
pub enum Permission {
    #[serde(rename = "memory.read")]
    #[strum(serialize = "memory.read")]
    MemoryRead,
    #[serde(rename = "memory.write")]
    #[strum(serialize = "memory.write")]
    MemoryWrite,
    #[serde(rename = "sessions.read")]
    #[strum(serialize = "sessions.read")]
    SessionsRead,
    #[serde(rename = "sessions.control")]
    #[strum(serialize = "sessions.control")]
    SessionsControl,
    #[serde(rename = "topics.read")]
    #[strum(serialize = "topics.read")]
    TopicsRead,
    #[serde(rename = "topics.write")]
    #[strum(serialize = "topics.write")]
    TopicsWrite,
    #[serde(rename = "notifications.send")]
    #[strum(serialize = "notifications.send")]
    NotificationsSend,
    #[serde(rename = "host.files.read")]
    #[strum(serialize = "host.files.read")]
    HostFilesRead,
    #[serde(rename = "host.files.write")]
    #[strum(serialize = "host.files.write")]
    HostFilesWrite,
    #[serde(rename = "host.network")]
    #[strum(serialize = "host.network")]
    HostNetwork,
    #[serde(rename = "host.exec")]
    #[strum(serialize = "host.exec")]
    HostExec,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginTool {
    pub name: ToolName,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: JsonObject,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<PluginToolHint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PluginToolHint {
    ReadOnly,
    Destructive,
    Idempotent,
    OpenWorld,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginContributions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<PluginPage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topic_cards: Vec<PluginTopicCard>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_actions: Vec<PluginSessionAction>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginPage {
    pub id: RouteName,
    pub title: String,
    pub icon: Option<String>,
    pub view: RouteName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginTopicCard {
    pub id: RouteName,
    pub title: String,
    pub view: RouteName,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginSessionAction {
    pub id: RouteName,
    pub title: String,
    pub icon: Option<String>,
    pub action: ViewAction,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginJob {
    pub id: RouteName,
    pub title: String,
    pub schedule: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum PluginEventKind {
    #[serde(rename = "session.started")]
    SessionStarted,
    #[serde(rename = "session.idle")]
    SessionIdle,
    #[serde(rename = "session.ended")]
    SessionEnded,
    #[serde(rename = "session.placed")]
    SessionPlaced,
    #[serde(rename = "summary.written")]
    SummaryWritten,
}
