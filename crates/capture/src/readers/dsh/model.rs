use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::value::RawValue;
use serde_with::DefaultOnError;
use serde_with::serde_as;

#[serde_as]
#[derive(Debug, Deserialize)]
pub(super) struct Line {
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "type", default)]
    pub(super) kind: Option<EventKind>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) seq: Option<i64>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) time: Option<i64>,
    #[serde(default)]
    pub(super) data: Option<Data>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "parentSession", default)]
    pub(super) parent_session: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) origin: Option<Origin>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Origin {
    Subagent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(super) enum EventKind {
    #[serde(rename = "session")]
    Session,
    #[serde(rename = "user/message")]
    UserMessage,
    #[serde(rename = "assistant/message")]
    AssistantMessage,
    #[serde(rename = "tool/call")]
    ToolCall,
    #[serde(rename = "tool/result")]
    ToolResult,
    #[serde(rename = "turn/end")]
    TurnEnd,
    #[serde(rename = "compaction/start")]
    CompactionStart,
    #[serde(rename = "session/title")]
    SessionTitle,
    #[serde(other)]
    Other,
}

#[serde_as]
#[derive(Debug, Default, Deserialize)]
pub(super) struct Data {
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "callId", default)]
    pub(super) call_id: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "call_id", default)]
    pub(super) call_id_snake: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) preset: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) source: Option<Source>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) reason: Option<Reason>,
    #[serde(default)]
    pub(super) message: Option<Message>,
    #[serde(default)]
    pub(super) content: Option<Box<RawValue>>,
    #[serde(default)]
    pub(super) arguments: Option<Box<RawValue>>,
    #[serde(default)]
    pub(super) args: Option<Box<RawValue>>,
    #[serde(default)]
    pub(super) error: Option<IgnoredAny>,
}

#[serde_as]
#[derive(Debug, Deserialize)]
pub(super) struct Message {
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "callId", default)]
    pub(super) call_id: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "call_id", default)]
    pub(super) call_id_snake: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) source: Option<Source>,
    #[serde(default)]
    pub(super) content: Option<Box<RawValue>>,
}

#[serde_as]
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct Source {
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) kind: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "callId", default)]
    pub(super) call_id: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "call_id", default)]
    pub(super) call_id_snake: Option<String>,
}

#[serde_as]
#[derive(Debug, Deserialize)]
pub(super) struct Reason {
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) kind: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum Content {
    Text(String),
    Parts(Vec<Part>),
}

#[serde_as]
#[derive(Debug, Deserialize)]
pub(super) struct Part {
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "type", default)]
    pub(super) kind: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) text: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) think: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) reasoning: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    pub(super) content: Option<Content>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "isError", default)]
    pub(super) is_error: Option<bool>,
}
