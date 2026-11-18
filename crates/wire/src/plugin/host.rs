use jiff::Timestamp;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::events::ProtocolVersion;
use crate::plugin::manifest::{Permission, PluginManifest, PluginTool};
use crate::plugin::view::ViewValue;
use crate::plugin::{PluginId, RouteName};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginList {
    pub node: String,
    pub plugins: Vec<PluginRecord>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginRecord {
    pub id: PluginId,
    pub node: String,
    pub path: String,
    pub manifest: PluginManifest,
    pub enabled: bool,
    pub state: PluginState,
    pub granted: Vec<Permission>,
    pub pending: Vec<Permission>,
    pub error: Option<String>,
    pub last_exit: Option<PluginExit>,
    pub restarts: u32,
    #[serde_as(as = "Option<crate::time::EpochMillis>")]
    pub started_at: Option<Timestamp>,
    #[serde_as(as = "crate::time::EpochMillis")]
    pub installed_at: Timestamp,
    pub jobs: Vec<PluginJobStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PluginState {
    Disabled,
    AwaitingApproval,
    Starting,
    Running,
    Backoff,
    Invalid,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginExit {
    #[serde_as(as = "crate::time::EpochMillis")]
    pub at: Timestamp,
    pub code: Option<i32>,
    pub signal: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginJobStatus {
    pub id: RouteName,
    pub last_run: Option<PluginJobRun>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginJobRun {
    pub run_id: String,
    pub trigger: PluginJobTrigger,
    #[serde_as(as = "crate::time::EpochMillis")]
    pub started_at: Timestamp,
    #[serde_as(as = "Option<crate::time::EpochMillis>")]
    pub finished_at: Option<Timestamp>,
    pub outcome: Option<PluginJobOutcome>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PluginJobTrigger {
    Schedule,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PluginJobOutcome {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginInstallRequest {
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginEnableRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approve: Vec<Permission>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginLogs {
    pub lines: Vec<PluginLogLine>,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginLogLine {
    #[serde_as(as = "crate::time::EpochMillis")]
    pub at: Timestamp,
    pub stream: PluginLogStream,
    pub text: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, strum::Display,
)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "camelCase")]
pub enum PluginLogStream {
    Stdout,
    Stderr,
    Host,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginSettings {
    pub values: BTreeMap<String, ViewValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets_set: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginSettingsUpdate {
    pub values: BTreeMap<String, Option<ViewValue>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename = "plugin.changed", rename_all = "camelCase")]
pub struct PluginChangedNotice {
    pub v: ProtocolVersion,
    pub plugin: PluginId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename = "plugin.views", rename_all = "camelCase")]
pub struct PluginViewsNotice {
    pub v: ProtocolVersion,
    pub plugin: PluginId,
    pub views: Vec<RouteName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginToolList {
    pub version: u64,
    pub tools: Vec<PluginToolEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginToolEntry {
    pub plugin: PluginId,
    pub tool: PluginTool,
}
