use std::collections::BTreeSet;
use std::time::Duration;

use async_trait::async_trait;
use values::{PluginId, PluginNameError, ToolName};

mod tools;

pub use tools::{
    PluginTool, PluginToolCall, PluginToolEntry, PluginToolHint, PluginToolList, PluginToolResult,
};

const TOOL_SEPARATOR: char = '-';
const FIRST_RESTART: Duration = Duration::from_secs(1);
const LAST_RESTART: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, strum::Display)]
pub enum Permission {
    #[strum(serialize = "memory.read")]
    MemoryRead,
    #[strum(serialize = "memory.write")]
    MemoryWrite,
    #[strum(serialize = "sessions.read")]
    SessionsRead,
    #[strum(serialize = "sessions.control")]
    SessionsControl,
    #[strum(serialize = "topics.read")]
    TopicsRead,
    #[strum(serialize = "topics.write")]
    TopicsWrite,
    #[strum(serialize = "tasks.read")]
    TasksRead,
    #[strum(serialize = "tasks.delegate")]
    TasksDelegate,
    #[strum(serialize = "agents.read")]
    AgentsRead,
    #[strum(serialize = "notifications.send")]
    NotificationsSend,
    #[strum(serialize = "host.files.read")]
    HostFilesRead,
    #[strum(serialize = "host.files.write")]
    HostFilesWrite,
    #[strum(serialize = "host.network")]
    HostNetwork,
    #[strum(serialize = "host.exec")]
    HostExec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    SessionStarted,
    SessionIdle,
    SessionEnded,
    SessionPlaced,
    SummaryWritten,
    TaskDone,
}

#[derive(Debug, thiserror::Error)]
pub enum PluginToolError {
    #[error("{0} is not a plug-in tool")]
    NotPluginTool(String),
    #[error(transparent)]
    Name(#[from] PluginNameError),
    #[error("plug-in {0} has no tool named {1}")]
    UnknownTool(String, String),
    #[error("plug-in {0} is not running")]
    NotRunning(String),
    #[error("plug-in {plugin} did not answer {tool} in time")]
    Timeout { plugin: String, tool: String },
    #[error("plug-in {plugin} answered {tool} with an invalid body: {detail}")]
    Invalid {
        plugin: String,
        tool: String,
        detail: String,
    },
    #[error("plug-in {plugin} failed {tool}: {detail}")]
    Failed {
        plugin: String,
        tool: String,
        detail: String,
    },
    #[error("the plug-in host is unreachable: {0}")]
    Unreachable(String),
}

#[async_trait]
pub trait PluginTools: Send + Sync {
    async fn tools(&self) -> Result<PluginToolList, PluginToolError>;

    async fn call(
        &self,
        plugin: &PluginId,
        tool: &ToolName,
        call: PluginToolCall,
        tool_use: Option<&str>,
    ) -> Result<PluginToolResult, PluginToolError>;

    async fn changed(&self, seen: u64) -> Result<u64, PluginToolError>;
}

#[must_use]
pub fn tool_name(plugin: &PluginId, tool: &ToolName) -> String {
    format!("{}{TOOL_SEPARATOR}{}", plugin.as_ref(), tool.as_ref())
}

pub fn split_tool_name(name: &str) -> Result<(PluginId, ToolName), PluginToolError> {
    let (plugin, tool) = name
        .rsplit_once(TOOL_SEPARATOR)
        .ok_or_else(|| PluginToolError::NotPluginTool(name.to_owned()))?;
    Ok((
        PluginId::try_from(plugin.to_owned())?,
        ToolName::try_from(tool.to_owned())?,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginAccess {
    Own,
    Needs(Permission),
    Refused,
}

#[must_use]
pub fn access(read: bool, path: &str) -> PluginAccess {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let needs = PluginAccess::Needs;
    match (read, segments.as_slice()) {
        (false, ["api", "plugin", "notify"]) => needs(Permission::NotificationsSend),
        (true, ["api", "plugin", "self"]) | (false, ["api", "plugin", "views", "changed"]) => {
            PluginAccess::Own
        }
        (false, ["api", "memory", "tool", "memory_append"] | ["api", "capture"]) => {
            needs(Permission::MemoryWrite)
        }
        (
            false,
            [
                "api",
                "memory",
                "tool",
                "memory_search" | "memory_browse" | "memory_stats" | "memory_get_full",
            ],
        )
        | (true, ["api", "memory" | "wiki", ..]) => needs(Permission::MemoryRead),
        (true, ["api", "harness-sessions" | "harnesses", ..]) => needs(Permission::SessionsRead),
        (false, ["api", "harness-sessions", ..] | ["api", "sessions", _, "messages"]) => {
            needs(Permission::SessionsControl)
        }
        (
            true,
            ["api", "topics", ..]
            | ["api", "sessions", "history" | "suggestions"]
            | ["api", "sessions", _, "placement"],
        ) => needs(Permission::TopicsRead),
        (false, ["api", "topics", ..] | ["api", "sessions", _, "placement" | "history"]) => {
            needs(Permission::TopicsWrite)
        }
        (true, ["api", "tasks", ..]) => needs(Permission::TasksRead),
        (false, ["api", "tasks"]) => needs(Permission::TasksDelegate),
        (true, ["api", "agents", ..] | ["api", "catalog", "agents"]) => {
            needs(Permission::AgentsRead)
        }
        _ => PluginAccess::Refused,
    }
}

#[must_use]
pub const fn event_permission(kind: EventKind) -> Permission {
    match kind {
        EventKind::SessionStarted
        | EventKind::SessionIdle
        | EventKind::SessionEnded
        | EventKind::SessionPlaced => Permission::SessionsRead,
        EventKind::SummaryWritten => Permission::MemoryRead,
        EventKind::TaskDone => Permission::TasksRead,
    }
}

#[must_use]
pub fn pending(requested: &[Permission], granted: &[Permission]) -> Vec<Permission> {
    let granted: BTreeSet<_> = granted.iter().collect();
    let requested: BTreeSet<_> = requested.iter().collect();
    requested
        .into_iter()
        .filter(|permission| !granted.contains(permission))
        .copied()
        .collect()
}

#[must_use]
pub fn narrowed(requested: &[Permission], granted: &[Permission]) -> Vec<Permission> {
    let requested: BTreeSet<_> = requested.iter().collect();
    let kept: BTreeSet<_> = granted
        .iter()
        .filter(|permission| requested.contains(permission))
        .copied()
        .collect();
    kept.into_iter().collect()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApprovalError {
    #[error("approve every pending permission: {}", names(.0))]
    Unapproved(Vec<Permission>),
    #[error("not pending: {}", names(.0))]
    NotPending(Vec<Permission>),
}

fn names(permissions: &[Permission]) -> String {
    permissions
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn approve(
    requested: &[Permission],
    granted: &[Permission],
    approved: &[Permission],
) -> Result<Vec<Permission>, ApprovalError> {
    let waiting = pending(requested, granted);
    let extra: Vec<Permission> = approved
        .iter()
        .filter(|permission| !waiting.contains(permission))
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if !extra.is_empty() {
        return Err(ApprovalError::NotPending(extra));
    }
    let missing: Vec<Permission> = waiting
        .iter()
        .filter(|permission| !approved.contains(permission))
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(ApprovalError::Unapproved(missing));
    }
    let all: BTreeSet<_> = requested.iter().copied().collect();
    Ok(all.into_iter().collect())
}

#[must_use]
pub fn restart_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(6);
    FIRST_RESTART
        .saturating_mul(1 << doublings)
        .min(LAST_RESTART)
}
