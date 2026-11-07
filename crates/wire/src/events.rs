use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged, try_from = "crate::JsonObject")]
pub enum SessionBroadcast {
    HubState(crate::gateway::hub_state::HubStateChanged),
    PluginChanged(crate::plugin::host::PluginChangedNotice),
    PluginViews(crate::plugin::host::PluginViewsNotice),
    TopicChanged(crate::gateway::topics::TopicChangedNotice),
}

#[derive(Deserialize)]
enum BroadcastKind {
    #[serde(rename = "hub.state")]
    HubState,
    #[serde(rename = "plugin.changed")]
    PluginChanged,
    #[serde(rename = "plugin.views")]
    PluginViews,
    #[serde(rename = "topic.changed")]
    TopicChanged,
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
            BroadcastKind::HubState => frame.deserialize().map(Self::HubState),
            BroadcastKind::PluginChanged => frame.deserialize().map(Self::PluginChanged),
            BroadcastKind::PluginViews => frame.deserialize().map(Self::PluginViews),
            BroadcastKind::TopicChanged => frame.deserialize().map(Self::TopicChanged),
        }
    }
}
