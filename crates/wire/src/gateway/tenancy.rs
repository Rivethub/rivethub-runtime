use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(schema_with = "space_kind_schema")]
pub enum SpaceKind {
    #[serde(rename = "household", alias = "device")]
    Device,
    Team,
    Compartment,
}

fn space_kind_schema(generator: &schemars::SchemaGenerator) -> schemars::Schema {
    if generator.contract().is_deserialize() {
        schemars::json_schema!({
            "type": "string",
            "enum": ["household", "device", "team", "compartment"]
        })
    } else {
        schemars::json_schema!({
            "type": "string",
            "enum": ["household", "team", "compartment"]
        })
    }
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpaceConfig {
    pub kind: SpaceKind,
    #[serde(default)]
    pub name: Option<String>,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpaceRecord {
    pub id: String,
    pub kind: SpaceKind,
    pub name: String,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserSettings {
    pub default_visibility: String,
    #[serde(default)]
    pub chosen: bool,
    #[serde(default)]
    #[serde_as(as = "Option<crate::time::EpochMillis>")]
    pub default_since: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserSettingsUpdate {
    pub default_visibility: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeResponse {
    pub user: String,
    pub spaces: Vec<SpaceRecord>,
    pub settings: UserSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<MeAccess>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeAccess {
    pub scopes: Vec<String>,
    pub drops_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibilityUpdate {
    #[serde(default)]
    pub visibility: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionVisibilityRecord {
    pub session_id: crate::session_id::SessionKey,
    pub visibility: String,
    #[serde(default)]
    pub mark: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicVisibilityRecord {
    pub topic_id: String,
    pub visibility: String,
    #[serde(default)]
    pub mark: Option<String>,
}
