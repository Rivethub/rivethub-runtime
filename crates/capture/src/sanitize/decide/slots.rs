use serde::Deserialize;
use serde::Serialize;
use wire::JsonObject;
use wire::capture::BundledSession;
use wire::json::Scrub;
use wire::json::member_path;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, strum::Display,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum FieldName {
    Title,
    Settings,
    Channel,
    TaskId,
    Content,
    ToolName,
    ToolResult,
    ToolArgs,
    Metadata,
}

impl FieldName {
    const fn scored(self) -> bool {
        !matches!(self, Self::Channel | Self::TaskId | Self::ToolName)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FieldAt {
    pub session: usize,
    pub message: Option<usize>,
    pub name: FieldName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseValue {
    Text(String),
    Object(JsonObject),
}

pub(super) enum FieldMut<'a> {
    Text(&'a mut String),
    Object(&'a mut JsonObject),
}

impl FieldMut<'_> {
    pub(super) fn base(&self) -> BaseValue {
        match self {
            Self::Text(text) => BaseValue::Text((**text).clone()),
            Self::Object(object) => BaseValue::Object((**object).clone()),
        }
    }

    pub(super) fn set(&mut self, base: &BaseValue) {
        match (self, base) {
            (Self::Text(text), BaseValue::Text(value)) => value.clone_into(text),
            (Self::Object(object), BaseValue::Object(value)) => value.clone_into(object),
            (Self::Text(_), BaseValue::Object(_)) | (Self::Object(_), BaseValue::Text(_)) => {}
        }
    }
}

pub(super) struct Slot<'a> {
    pub ordinal: usize,
    pub path: &'a str,
    pub key: bool,
}

impl Slot<'_> {
    pub(super) fn shown(&self, name: FieldName) -> String {
        let field = if self.path.is_empty() {
            name.to_string()
        } else {
            format!("{name}.{}", self.path)
        };
        if self.key {
            format!("{field} (key)")
        } else {
            field
        }
    }
}

pub(super) struct Owner<'a> {
    pub session: &'a str,
    pub id: &'a str,
}

pub(super) fn fields(
    sessions: &mut [BundledSession],
    visit: &mut impl FnMut(FieldAt, &Owner<'_>, FieldMut<'_>),
) {
    for (session, bundled) in sessions.iter_mut().enumerate() {
        let key = bundled.origin.session_key.as_str();
        let owner = &Owner {
            session: key,
            id: key,
        };
        let batch = &mut bundled.batch;
        let at = |name| FieldAt {
            session,
            message: None,
            name,
        };
        if let Some(title) = &mut batch.title {
            visit(at(FieldName::Title), owner, FieldMut::Text(title));
        }
        if let Some(settings) = &mut batch.settings {
            visit(at(FieldName::Settings), owner, FieldMut::Object(settings));
        }
        if let Some(channel) = &mut batch.channel {
            visit(at(FieldName::Channel), owner, FieldMut::Text(channel));
        }
        if let Some(task) = &mut batch.task_id {
            visit(at(FieldName::TaskId), owner, FieldMut::Text(task));
        }
        for (position, message) in batch.messages.iter_mut().enumerate() {
            let owner = &Owner {
                session: key,
                id: message.event_id.as_str(),
            };
            let at = |name| FieldAt {
                session,
                message: Some(position),
                name,
            };
            visit(
                at(FieldName::Content),
                owner,
                FieldMut::Text(&mut message.content),
            );
            if let Some(name) = &mut message.tool_name {
                visit(at(FieldName::ToolName), owner, FieldMut::Text(name));
            }
            if let Some(result) = &mut message.tool_result {
                visit(at(FieldName::ToolResult), owner, FieldMut::Text(result));
            }
            if let Some(arguments) = &mut message.tool_args {
                visit(at(FieldName::ToolArgs), owner, FieldMut::Object(arguments));
            }
            if let Some(metadata) = &mut message.metadata {
                visit(at(FieldName::Metadata), owner, FieldMut::Object(metadata));
            }
        }
    }
}

pub(super) fn slots(field: &mut FieldMut<'_>, visit: &mut impl FnMut(&Slot<'_>, &mut String)) {
    match field {
        FieldMut::Text(text) => visit(
            &Slot {
                ordinal: 0,
                path: "",
                key: false,
            },
            text,
        ),
        FieldMut::Object(object) => object.scrub(&mut Walk { next: 0, visit }),
    }
}

pub(super) fn scored(
    sessions: &mut [BundledSession],
    visit: &mut impl FnMut(&FieldAt, &str, &str),
) {
    fields(sessions, &mut |at, _, mut field| {
        if at.name.scored() {
            slots(&mut field, &mut |slot, text| {
                if !slot.key {
                    visit(&at, slot.path, text);
                }
            });
        }
    });
}

struct Walk<'v, F> {
    next: usize,
    visit: &'v mut F,
}

impl<F: FnMut(&Slot<'_>, &mut String)> Walk<'_, F> {
    fn slot(&mut self, path: &str, key: bool, text: &mut String) {
        let slot = Slot {
            ordinal: self.next,
            path,
            key,
        };
        self.next += 1;
        (self.visit)(&slot, text);
    }
}

impl<F: FnMut(&Slot<'_>, &mut String)> Scrub for Walk<'_, F> {
    fn is_secret_name(&self, _key: &str) -> bool {
        false
    }

    fn rank(&self, _key: &str) -> u64 {
        0
    }

    fn key(&mut self, parent: &str, key: &mut String) -> usize {
        let path = member_path(parent, key);
        self.slot(&path, true, key);
        0
    }

    fn renamed(
        &mut self,
        _parent: &str,
        _markers: (usize, Option<usize>),
        _key: &str,
        _renamed: &str,
    ) {
    }

    fn text(&mut self, path: &str, text: &mut String) {
        self.slot(path, false, text);
    }

    fn secret(&mut self, _path: &str, _key: &str, _text: &mut String) -> bool {
        false
    }
}
