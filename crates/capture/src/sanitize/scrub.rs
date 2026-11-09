use std::ops::Range;

use sha2::Digest as _;
use sha2::Sha256;
use wire::JsonObject;
use wire::capture::BundledSession;
use wire::json::Current;
use wire::json::Scrub;

use super::mapping::Action;
use super::mapping::Mapping;
use super::report::Change;
use super::report::Collision;
use super::report::Field;
use super::report::RuleId;
use super::report::Spot;
use crate::redaction;
use crate::rules::Ruleset;

const FINGERPRINT_HEX: usize = 12;
const PLACEHOLDER_OPEN: &str = "[REDACTED";
const PATTERN_KIND: &str = "pattern:";

#[derive(Debug, Clone, Default)]
pub(super) struct Location {
    pub(super) session: Option<usize>,
    pub(super) message: Option<usize>,
}

pub(super) struct Scrubber<'a> {
    mapping: &'a Mapping,
    rules: &'static Ruleset,
    key: &'a str,
    pub(super) at: Location,
    pub(super) changes: Vec<Change>,
    pub(super) collisions: Vec<Collision>,
}

struct Hit {
    range: Range<usize>,
    rule: RuleId,
    replacement: String,
}

impl<'a> Scrubber<'a> {
    pub(super) fn new(mapping: &'a Mapping, key: &'a str) -> Self {
        Self {
            mapping,
            rules: crate::rules::ruleset(),
            key,
            at: Location::default(),
            changes: Vec::new(),
            collisions: Vec::new(),
        }
    }

    pub(super) fn session(&mut self, index: usize, session: &mut BundledSession) {
        self.at = Location {
            session: Some(index),
            message: None,
        };
        self.text(&Field::OriginNode, &mut session.origin.node);
        self.text(&Field::OriginSessionKey, &mut session.origin.session_key);
        self.text(&Field::OriginAgent, &mut session.origin.agent);
        let batch = &mut session.batch;
        self.text(&Field::SessionKey, &mut batch.session_key);
        self.text(&Field::Agent, &mut batch.agent);
        if let Some(channel) = &mut batch.channel {
            self.text(&Field::Channel, channel);
        }
        if let Some(task_id) = &mut batch.task_id {
            self.text(&Field::TaskId, task_id);
        }
        if let Some(title) = &mut batch.title {
            self.text(&Field::Title, title);
        }
        if let Some(settings) = &mut batch.settings {
            self.object(Field::Settings, settings);
        }
        for (position, message) in batch.messages.iter_mut().enumerate() {
            self.at.message = Some(position);
            self.text(&Field::Content, &mut message.content);
            if let Some(result) = &mut message.tool_result {
                self.text(&Field::ToolResult, result);
            }
            if let Some(name) = &mut message.tool_name {
                self.text(&Field::ToolName, name);
            }
            if let Some(arguments) = &mut message.tool_args {
                self.object(Field::ToolArgs, arguments);
            }
            if let Some(metadata) = &mut message.metadata {
                self.object(Field::Metadata, metadata);
            }
        }
    }

    fn object(&mut self, field: fn(Spot) -> Field, object: &mut JsonObject) {
        let mut adapter = ObjectScrub {
            scrubber: self,
            field,
        };
        object.scrub(&mut adapter);
    }

    pub(super) fn text(&mut self, field: &Field, text: &mut String) {
        let rules = self.rules;
        self.apply(field, text, |masked| {
            rules
                .spans(masked)
                .into_iter()
                .map(|span| Hit {
                    range: span.range,
                    rule: RuleId::Pattern(&span.rule.id),
                    replacement: redaction::placeholder(&span.rule.id),
                })
                .collect()
        });
        if self.mapping.is_empty() {
            return;
        }
        let mapping = self.mapping;
        self.apply(field, text, |masked| {
            mapping
                .spans(masked)
                .into_iter()
                .map(|(range, rule)| Hit {
                    range,
                    rule: match rule.action {
                        Action::Map(_) => RuleId::Mapped { line: rule.line },
                        Action::Redact => RuleId::Redacted { line: rule.line },
                    },
                    replacement: rule.action.replacement().to_owned(),
                })
                .collect()
        });
    }

    fn apply(&mut self, field: &Field, text: &mut String, hits: impl Fn(&str) -> Vec<Hit>) {
        let protected = protected(text, self.rules);
        let masked = mask(text, &protected);
        let found = hits(&masked);
        let mut out = String::with_capacity(text.len());
        let mut copied = 0;
        for hit in found {
            if hit.range.start < copied
                || protected
                    .iter()
                    .any(|range| hit.range.start < range.end && range.start < hit.range.end)
            {
                continue;
            }
            let Some(original) = text.get(hit.range.clone()) else {
                self.record(field.clone(), hit.rule, text, &hit.replacement);
                *text = hit.replacement;
                return;
            };
            out.push_str(text.get(copied..hit.range.start).unwrap_or_default());
            self.record(field.clone(), hit.rule, original, &hit.replacement);
            out.push_str(&hit.replacement);
            copied = hit.range.end;
        }
        if copied == 0 {
            return;
        }
        out.push_str(text.get(copied..).unwrap_or_default());
        *text = out;
    }

    fn record(&mut self, field: Field, rule: RuleId, original: &str, replacement: &str) {
        let mut hasher = Sha256::new();
        hasher.update(self.key.as_bytes());
        hasher.update([0]);
        hasher.update(original.as_bytes());
        let mut fingerprint = hex::encode(hasher.finalize());
        fingerprint.truncate(FINGERPRINT_HEX);
        self.changes.push(Change {
            session: self.at.session,
            message: self.at.message,
            event_id: None,
            field,
            rule,
            fingerprint,
            replacement: replacement.to_owned(),
        });
    }
}

struct ObjectScrub<'s, 'a> {
    scrubber: &'s mut Scrubber<'a>,
    field: fn(Spot) -> Field,
}

impl Scrub for ObjectScrub<'_, '_> {
    fn is_secret_name(&self, key: &str) -> bool {
        self.scrubber.rules.is_secret_name(key)
    }

    fn key(&mut self, parent: &str, key: &mut String) {
        let field = (self.field)(Spot {
            path: parent.to_owned(),
            key: true,
        });
        self.scrubber.text(&field, key);
    }

    fn text(&mut self, path: &str, text: &mut String) {
        let field = (self.field)(Spot {
            path: path.to_owned(),
            key: false,
        });
        self.scrubber.text(&field, text);
    }

    fn whole(&mut self, path: &str, current: Current<'_>) -> Option<String> {
        let original = match current {
            Current::Text(text) if text == redaction::SECRET_KEY_PLACEHOLDER => return None,
            Current::Text(text) => text.to_owned(),
            Current::Json(json) => json,
        };
        let field = (self.field)(Spot {
            path: path.to_owned(),
            key: false,
        });
        self.scrubber.record(
            field,
            RuleId::SecretKey,
            &original,
            redaction::SECRET_KEY_PLACEHOLDER,
        );
        Some(redaction::SECRET_KEY_PLACEHOLDER.to_owned())
    }

    fn collision(&mut self, parent: &str, key: &str) {
        let field = (self.field)(Spot {
            path: parent.to_owned(),
            key: true,
        });
        self.scrubber.collisions.push(Collision {
            session: self.scrubber.at.session,
            message: self.scrubber.at.message,
            event_id: None,
            field,
            key: key.to_owned(),
        });
    }
}

fn protected(text: &str, rules: &Ruleset) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut from = 0;
    while let Some(found) = text
        .get(from..)
        .and_then(|rest| rest.find(PLACEHOLDER_OPEN))
    {
        let start = from + found;
        from = start + PLACEHOLDER_OPEN.len();
        if let Some(end) = placeholder_end(text, from, rules) {
            ranges.push(start..end);
            from = end;
        }
    }
    ranges
}

fn mask(text: &str, protected: &[Range<usize>]) -> String {
    if protected.is_empty() {
        return text.to_owned();
    }
    let mut masked = String::with_capacity(text.len());
    let mut copied = 0;
    for range in protected {
        masked.push_str(text.get(copied..range.start).unwrap_or_default());
        masked.extend(std::iter::repeat_n(' ', range.len()));
        copied = range.end;
    }
    masked.push_str(text.get(copied..).unwrap_or_default());
    masked
}

fn placeholder_end(text: &str, after_open: usize, rules: &Ruleset) -> Option<usize> {
    let rest = text.get(after_open..)?;
    if rest.starts_with(']') {
        return Some(after_open + 1);
    }
    let kind = rest.strip_prefix(':')?;
    let length = kind.find(']')?;
    let kind = kind.get(..length)?;
    let emitted = kind == redaction::SECRET_KEY_KIND
        || rules.emits(kind)
        || kind
            .strip_prefix(PATTERN_KIND)
            .is_some_and(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()));
    emitted.then_some(after_open + 1 + length + 1)
}
