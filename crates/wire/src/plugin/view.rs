use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::JsonObject;
use crate::plugin::RouteName;
use crate::plugin::action::ViewAction;
use crate::plugin::canvas::{CanvasMenuItem, CanvasScene};
use crate::session_id::SessionId;

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct View {
    pub title: String,
    pub subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toolbar: Vec<ViewButton>,
    pub blocks: Vec<ViewBlock>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewRequest {
    pub params: Option<JsonObject>,
    #[serde(default)]
    pub context: ViewContext,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewContext {
    pub topic_id: Option<String>,
    pub session_id: Option<SessionId>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum ViewBlock {
    #[serde(rename_all = "camelCase")]
    Section {
        title: Option<String>,
        subtitle: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<ViewButton>,
        blocks: Vec<Self>,
        collapsed: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    Split {
        primary: Vec<Self>,
        secondary: Vec<Self>,
        ratio: Option<f64>,
    },
    #[serde(rename_all = "camelCase")]
    Tabs {
        tabs: Vec<ViewTab>,
        selected: Option<RouteName>,
    },
    #[serde(rename_all = "camelCase")]
    Text {
        text: String,
        tone: Option<ViewTone>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        monospace: bool,
    },
    #[serde(rename_all = "camelCase")]
    Markdown { text: String },
    #[serde(rename_all = "camelCase")]
    Code {
        text: String,
        language: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        follow: bool,
    },
    #[serde(rename_all = "camelCase")]
    Image { path: String, alt: String },
    #[serde(rename_all = "camelCase")]
    Properties { items: Vec<ViewProperty> },
    #[serde(rename_all = "camelCase")]
    Progress {
        value: Option<f64>,
        label: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Notice {
        tone: ViewTone,
        title: Option<String>,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<ViewButton>,
    },
    #[serde(rename_all = "camelCase")]
    List {
        items: Vec<ViewListItem>,
        empty: Option<ViewEmpty>,
    },
    #[serde(rename_all = "camelCase")]
    Table {
        columns: Vec<ViewColumn>,
        rows: Vec<ViewRow>,
        empty: Option<ViewEmpty>,
    },
    #[serde(rename_all = "camelCase")]
    Breadcrumbs { items: Vec<ViewCrumb> },
    #[serde(rename_all = "camelCase")]
    Actions { buttons: Vec<ViewButton> },
    #[serde(rename_all = "camelCase")]
    Form {
        id: RouteName,
        fields: Vec<ViewField>,
        buttons: Vec<ViewButton>,
    },
    #[serde(rename_all = "camelCase")]
    Canvas {
        id: RouteName,
        scene: CanvasScene,
        height: Option<u32>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        read_only: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        menu: Vec<CanvasMenuItem>,
    },
    #[serde(rename_all = "camelCase")]
    WebPanel {
        id: RouteName,
        entry: String,
        height: Option<u32>,
        params: Option<JsonObject>,
    },
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewTab {
    pub id: RouteName,
    pub title: String,
    pub badge: Option<String>,
    pub blocks: Vec<ViewBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewProperty {
    pub label: String,
    pub value: ViewInline,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewListItem {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trailing: Vec<ViewInline>,
    pub action: Option<ViewAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub menu: Vec<ViewButton>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewColumn {
    pub title: String,
    pub align: Option<ViewAlign>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sortable: bool,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewRow {
    pub id: String,
    pub cells: Vec<ViewInline>,
    pub action: Option<ViewAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub menu: Vec<ViewButton>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewEmpty {
    pub title: String,
    pub text: Option<String>,
    pub button: Option<ViewButton>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewCrumb {
    pub label: String,
    pub action: Option<ViewAction>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum ViewInline {
    #[serde(rename_all = "camelCase")]
    Text {
        text: String,
        tone: Option<ViewTone>,
        icon: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        monospace: bool,
    },
    #[serde(rename_all = "camelCase")]
    Badge { text: String, tone: ViewTone },
    #[serde(rename_all = "camelCase")]
    Time {
        #[serde_as(as = "crate::time::EpochMillis")]
        at: Timestamp,
        style: Option<ViewTimeStyle>,
    },
    #[serde(rename_all = "camelCase")]
    Duration { ms: u64 },
    #[serde(rename_all = "camelCase")]
    Bytes { value: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ViewTone {
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ViewAlign {
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ViewTimeStyle {
    Relative,
    Absolute,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewButton {
    pub label: String,
    pub icon: Option<String>,
    pub style: Option<ViewButtonStyle>,
    pub action: ViewAction,
    pub confirm: Option<ViewConfirm>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ViewButtonStyle {
    Primary,
    Secondary,
    Danger,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewConfirm {
    pub title: String,
    pub text: String,
    pub confirm_label: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewField {
    pub key: String,
    pub label: String,
    pub input: ViewInput,
    pub value: Option<ViewValue>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    pub help: Option<String>,
    pub placeholder: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum ViewInput {
    #[serde(rename_all = "camelCase")]
    Text {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        multiline: bool,
    },
    Secret,
    #[serde(rename_all = "camelCase")]
    Number {
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
    },
    Toggle,
    #[serde(rename_all = "camelCase")]
    Select {
        options: Vec<ViewOption>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        multiple: bool,
    },
    #[serde(rename_all = "camelCase")]
    File {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        multiple: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        accept: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ViewValue {
    Bool(bool),
    Number(f64),
    Text(String),
    List(Vec<String>),
    Files(Vec<ViewFile>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewFile {
    pub name: String,
    pub mime_type: String,
    pub data: String,
}
