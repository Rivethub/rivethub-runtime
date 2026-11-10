use wire::capture::BundledSession;
use wire::json::Scrub;

use super::super::report::Field;
use super::super::report::Spot;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct At {
    pub session: usize,
    pub message: Option<usize>,
    pub field: String,
}

pub(super) struct Slot<'a> {
    pub at: At,
    pub owner: &'a str,
}

pub(super) fn visit(
    sessions: &mut [BundledSession],
    visit: &mut impl FnMut(Slot<'_>, &mut String),
) {
    for (index, session) in sessions.iter_mut().enumerate() {
        let owner = session.origin.session_key.as_str();
        let batch = &mut session.batch;
        let mut header = |field: &Field, text: &mut String| {
            visit(slot(index, None, owner, field), text);
        };
        if let Some(title) = &mut batch.title {
            header(&Field::Title, title);
        }
        if let Some(settings) = &mut batch.settings {
            visit_object(Field::Settings, settings, &mut header);
        }
        for (position, message) in batch.messages.iter_mut().enumerate() {
            let owner = message.event_id.as_str();
            let mut inner = |field: &Field, text: &mut String| {
                visit(slot(index, Some(position), owner, field), text);
            };
            inner(&Field::Content, &mut message.content);
            if let Some(result) = &mut message.tool_result {
                inner(&Field::ToolResult, result);
            }
            if let Some(arguments) = &mut message.tool_args {
                visit_object(Field::ToolArgs, arguments, &mut inner);
            }
            if let Some(metadata) = &mut message.metadata {
                visit_object(Field::Metadata, metadata, &mut inner);
            }
        }
    }
}

fn slot<'a>(session: usize, message: Option<usize>, owner: &'a str, field: &Field) -> Slot<'a> {
    Slot {
        at: At {
            session,
            message,
            field: field.to_string(),
        },
        owner,
    }
}

fn visit_object(
    field: fn(Spot) -> Field,
    object: &mut wire::JsonObject,
    visit: &mut impl FnMut(&Field, &mut String),
) {
    object.scrub(&mut Values { field, visit });
}

struct Values<'a, F> {
    field: fn(Spot) -> Field,
    visit: &'a mut F,
}

impl<F: FnMut(&Field, &mut String)> Scrub for Values<'_, F> {
    fn is_secret_name(&self, _key: &str) -> bool {
        false
    }

    fn rank(&self, _key: &str) -> u64 {
        0
    }

    fn key(&mut self, _parent: &str, _key: &mut String) -> usize {
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
        let spot = (self.field)(Spot {
            path: path.to_owned(),
            key: false,
        });
        (self.visit)(&spot, text);
    }

    fn secret(&mut self, _path: &str, _key: &str, _text: &mut String) -> bool {
        false
    }
}
