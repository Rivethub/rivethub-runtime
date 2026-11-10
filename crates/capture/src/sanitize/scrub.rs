use std::ops::Range;

use wire::JsonObject;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::json::Scrub;
use wire::json::member_path;

use super::key::LocalKey;
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Location {
    pub(super) session: Option<usize>,
    pub(super) message: Option<usize>,
}

pub(super) struct Scrubber<'a> {
    mapping: &'a Mapping,
    rules: &'static Ruleset,
    key: Option<&'a LocalKey>,
    digest: &'a str,
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
    pub(super) fn new(mapping: &'a Mapping, key: Option<&'a LocalKey>, digest: &'a str) -> Self {
        Self {
            mapping,
            rules: crate::rules::ruleset(),
            key,
            digest,
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
        self.secrets_only(&Field::OriginAuthor, &mut session.origin.author);
        self.text(&Field::OriginNode, &mut session.origin.node);
        self.text(
            &Field::OriginConversation,
            &mut session.origin.conversation_id,
        );
        self.text(&Field::OriginSessionKey, &mut session.origin.session_key);
        self.text(&Field::OriginAgent, &mut session.origin.agent);
        let batch = &mut session.batch;
        self.text(&Field::SessionKey, &mut batch.session_key);
        self.text(&Field::Agent, &mut batch.agent);
        if let Some(channel) = &mut batch.channel {
            self.text(&Field::Channel, channel);
        }
        if let Some(task_id) = &mut batch.task_id {
            self.task_id(task_id);
        }
        if let Some(title) = &mut batch.title {
            self.text(&Field::Title, title);
        }
        if let Some(settings) = &mut batch.settings {
            self.object(Field::Settings, settings);
        }
        for (position, message) in batch.messages.iter_mut().enumerate() {
            self.at.message = Some(position);
            self.secrets_only(&Field::EventId, &mut message.event_id);
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

    pub(super) fn manifest(&mut self, exported_by: &mut BundleAuthor) {
        self.at = Location::default();
        self.text(&Field::ExporterUser, &mut exported_by.user);
        self.text(&Field::ExporterNode, &mut exported_by.node);
    }

    fn object(&mut self, field: fn(Spot) -> Field, object: &mut JsonObject) {
        let mut adapter = ObjectScrub {
            scrubber: self,
            field,
        };
        object.scrub(&mut adapter);
    }

    fn secret_hits(&self, text: &str) -> Vec<Hit> {
        self.rules
            .spans(text)
            .into_iter()
            .map(|span| Hit {
                range: span.range,
                rule: RuleId::Pattern(&span.rule.id),
                replacement: redaction::placeholder(&span.rule.id),
            })
            .collect()
    }

    fn secrets_only(&mut self, field: &Field, text: &mut String) {
        let hits = self.secret_hits(text);
        self.apply(field, text, hits);
    }

    pub(super) fn text(&mut self, field: &Field, text: &mut String) {
        self.secrets_only(field, text);
        if self.mapping.is_empty() {
            return;
        }
        let hits = self
            .mapping
            .spans(text)
            .into_iter()
            .map(|(range, rule)| Hit {
                range,
                rule: match rule.action {
                    Action::Map(_) => RuleId::Mapped { line: rule.line },
                    Action::Redact => RuleId::Redacted { line: rule.line },
                },
                replacement: rule.action.replacement().to_owned(),
            })
            .collect();
        self.apply(field, text, hits);
    }

    fn task_id(&mut self, text: &mut String) {
        let hits = self.secret_hits(text);
        let Some(first) = hits.first() else {
            return;
        };
        let replacement = match self.key {
            Some(key) => key.uuid(&[b"task_id", text.as_bytes()]),
            None => first.replacement.clone(),
        };
        self.record(Field::TaskId, first.rule, text, &replacement);
        *text = replacement;
    }

    fn apply(&mut self, field: &Field, text: &mut String, hits: Vec<Hit>) {
        let protected = protected(text, self.rules);
        let mut out = String::with_capacity(text.len());
        let mut copied = 0;
        for hit in hits {
            for piece in unprotected(&hit.range, &protected) {
                if piece.start < copied {
                    continue;
                }
                let Some(original) = text.get(piece.clone()) else {
                    continue;
                };
                if original.trim().is_empty() {
                    continue;
                }
                out.push_str(text.get(copied..piece.start).unwrap_or_default());
                self.record(field.clone(), hit.rule, original, &hit.replacement);
                out.push_str(&hit.replacement);
                copied = piece.end;
            }
        }
        if copied == 0 {
            return;
        }
        out.push_str(text.get(copied..).unwrap_or_default());
        *text = out;
    }

    fn record(&mut self, field: Field, rule: RuleId, original: &str, replacement: &str) {
        let fingerprint = self.key.map_or_else(String::new, |key| {
            let mut hex = key.hex(&[self.digest.as_bytes(), original.as_bytes()]);
            hex.truncate(FINGERPRINT_HEX);
            hex
        });
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

impl ObjectScrub<'_, '_> {
    fn spot(&self, path: &str, key: bool) -> Field {
        (self.field)(Spot {
            path: path.to_owned(),
            key,
        })
    }
}

impl Scrub for ObjectScrub<'_, '_> {
    fn is_secret_name(&self, key: &str) -> bool {
        self.scrubber.rules.is_secret_name(key)
    }

    fn rank(&self, key: &str) -> u64 {
        self.scrubber.key.map_or(0, |local| {
            let mac = local.mac(&[b"collision", key.as_bytes()]);
            let mut prefix = [0u8; 8];
            prefix.copy_from_slice(&mac[..8]);
            u64::from_be_bytes(prefix)
        })
    }

    fn key(&mut self, parent: &str, key: &mut String) -> usize {
        let start = self.scrubber.changes.len();
        let provisional = self.spot(parent, true);
        self.scrubber.text(&provisional, key);
        let placed = self.spot(&member_path(parent, key), true);
        for change in self.scrubber.changes.iter_mut().skip(start) {
            change.field = placed.clone();
        }
        start
    }

    fn renamed(&mut self, parent: &str, markers: (usize, Option<usize>), key: &str, renamed: &str) {
        let from = self.spot(&member_path(parent, key), true);
        let to = self.spot(&member_path(parent, renamed), true);
        let (start, end) = markers;
        let end = end.unwrap_or(self.scrubber.changes.len());
        for change in self.scrubber.changes.iter_mut().take(end).skip(start) {
            if change.field == from {
                change.field = to.clone();
            }
        }
        let here = self.scrubber.at.clone();
        self.scrubber.collisions.push(Collision {
            session: here.session,
            message: here.message,
            event_id: None,
            field: to,
            key: renamed.to_owned(),
        });
    }

    fn text(&mut self, path: &str, text: &mut String) {
        let field = self.spot(path, false);
        self.scrubber.text(&field, text);
    }

    fn secret(&mut self, path: &str, key: &str, text: &mut String) -> bool {
        if text == redaction::SECRET_KEY_PLACEHOLDER
            || !self.scrubber.rules.names().redacts(key, text)
        {
            return false;
        }
        let field = self.spot(path, false);
        self.scrubber.record(
            field,
            RuleId::SecretKey,
            text,
            redaction::SECRET_KEY_PLACEHOLDER,
        );
        redaction::SECRET_KEY_PLACEHOLDER.clone_into(text);
        true
    }
}

fn unprotected(hit: &Range<usize>, protected: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut pieces = Vec::new();
    let mut start = hit.start;
    for range in protected {
        if range.end <= start || range.start >= hit.end {
            continue;
        }
        if range.start > start {
            pieces.push(start..range.start);
        }
        start = start.max(range.end);
    }
    if start < hit.end {
        pieces.push(start..hit.end);
    }
    pieces
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

fn placeholder_end(text: &str, after_open: usize, rules: &Ruleset) -> Option<usize> {
    let start = after_open.checked_sub(PLACEHOLDER_OPEN.len())?;
    let length = rules.names().placeholder_len(text.get(start..)?);
    (length > 0).then_some(start + length)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(list: &[(usize, usize)]) -> Vec<Range<usize>> {
        list.iter().map(|&(start, end)| start..end).collect()
    }

    #[test]
    fn a_hit_is_trimmed_to_the_spans_outside_protected_placeholders() {
        assert_eq!(unprotected(&(0..10), &[]), ranges(&[(0, 10)]));
        assert_eq!(
            unprotected(&(0..10), &ranges(&[(3, 5)])),
            ranges(&[(0, 3), (5, 10)])
        );
        assert_eq!(
            unprotected(&(0..10), &ranges(&[(0, 4), (8, 12)])),
            ranges(&[(4, 8)])
        );
        assert_eq!(unprotected(&(2..6), &ranges(&[(0, 10)])), ranges(&[]));
        assert_eq!(
            unprotected(&(0..10), &ranges(&[(10, 12)])),
            ranges(&[(0, 10)])
        );
    }
}
