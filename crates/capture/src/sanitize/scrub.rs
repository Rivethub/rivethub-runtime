use std::ops::Range;

use wire::JsonObject;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::CaptureMessage;
use wire::json::Scrub;
use wire::json::member_path;
use wire::message::Role;

use super::chosen::SessionRules;
use super::key::LocalKey;
use super::mapping::Action;
use super::mapping::Mapping;
use super::report::Change;
use super::report::Collision;
use super::report::Field;
use super::report::RuleId;
use super::report::Spot;
use crate::redaction;
use crate::rules::Budget;
use crate::rules::Ruleset;
use crate::rules::injection;
use crate::rules::names::DROP_TOOL_OUTPUTS_KIND;
use crate::rules::names::SESSION_TERM_KIND;

const FINGERPRINT_HEX: usize = 12;
const EXCERPTS_PER_TEXT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Posture {
    Sanitize,
    Receive,
}

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
    pub(super) budget: Budget<'a>,
    pub(super) posture: Posture,
    pub(super) whole: bool,
    pub(super) escape_flags: bool,
    pub(super) sessions: &'a SessionRules,
    pub(super) at: Location,
    pub(super) changes: Vec<Change>,
    pub(super) collisions: Vec<Collision>,
}

struct Hit {
    range: Range<usize>,
    rule: RuleId,
    replacement: String,
}

struct Shown {
    change: usize,
    at: Range<usize>,
    original: Range<usize>,
}

impl<'a> Scrubber<'a> {
    pub(super) fn new(mapping: &'a Mapping, key: Option<&'a LocalKey>, digest: &'a str) -> Self {
        Self {
            mapping,
            rules: crate::rules::ruleset(),
            key,
            digest,
            budget: Budget::fresh(),
            posture: Posture::Sanitize,
            whole: false,
            escape_flags: false,
            sessions: &super::chosen::NONE,
            at: Location::default(),
            changes: Vec::new(),
            collisions: Vec::new(),
        }
    }

    pub(super) fn session(&mut self, index: usize, session: &mut BundledSession) {
        self.header(index, session);
        for (position, message) in session.batch.messages.iter_mut().enumerate() {
            self.message(index, position, message);
        }
    }

    pub(super) fn header(&mut self, index: usize, session: &mut BundledSession) {
        self.at = Location {
            session: Some(index),
            message: None,
        };
        self.budget.next_message();
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
    }

    pub(super) fn message(
        &mut self,
        session: usize,
        position: usize,
        message: &mut CaptureMessage,
    ) {
        self.at = Location {
            session: Some(session),
            message: Some(position),
        };
        self.budget.next_message();
        self.secrets_only(&Field::EventId, &mut message.event_id);
        let drops_outputs = self.sessions.drops_tool_outputs(session);
        if drops_outputs && message.role == Role::Tool && !message.content.is_empty() {
            self.drop_output(Field::Content, &mut message.content);
        } else {
            self.text(&Field::Content, &mut message.content);
        }
        if let Some(result) = &mut message.tool_result {
            if drops_outputs {
                self.drop_output(Field::ToolResult, result);
            } else {
                self.text(&Field::ToolResult, result);
            }
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

    pub(super) fn manifest(&mut self, exported_by: &mut BundleAuthor) {
        self.at = Location::default();
        self.budget.next_message();
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

    fn placeholder(&self, kind: &str) -> String {
        match self.posture {
            Posture::Sanitize => redaction::placeholder(kind),
            Posture::Receive => redaction::held_placeholder(kind),
        }
    }

    fn unscanned(&self, text: &str) -> Vec<Hit> {
        if text.is_empty() {
            return Vec::new();
        }
        vec![Hit {
            range: 0..text.len(),
            rule: RuleId::Injection {
                id: injection::BUDGET_EXHAUSTED,
                family: injection::Family::Budget,
            },
            replacement: self.placeholder(injection::BUDGET_EXHAUSTED),
        }]
    }

    fn secret_hits(&mut self, text: &str) -> Vec<Hit> {
        if self.budget.exhausted() {
            return self.unscanned(text);
        }
        let mut hits: Vec<Hit> = self
            .rules
            .spans_within(text, &mut self.budget)
            .into_iter()
            .map(|span| Hit {
                range: span.range,
                rule: RuleId::Pattern(&span.rule.id),
                replacement: self.placeholder(&span.rule.id),
            })
            .collect();
        if self.budget.exhausted() {
            return self.unscanned(text);
        }
        if self.posture == Posture::Receive {
            let injected = injection::spans(text, &mut self.budget);
            hits.extend(injected.into_iter().map(|(span, family)| Hit {
                range: span.range,
                rule: RuleId::Injection {
                    id: &span.rule.id,
                    family,
                },
                replacement: self.placeholder(&span.rule.id),
            }));
            hits.sort_by_key(|hit| hit.range.start);
            if self.budget.exhausted() {
                return self.unscanned(text);
            }
            hits = overlapping_as_one(hits);
            hits = run_together(hits);
        }
        hits
    }

    fn secrets_only(&mut self, field: &Field, text: &mut String) {
        if self.escape_flags {
            self.escape_flags(field, text);
        }
        let hits = self.secret_hits(text);
        self.apply(field, text, hits);
    }

    pub(super) fn text(&mut self, field: &Field, text: &mut String) {
        self.secrets_only(field, text);
        let sessions = self.sessions;
        if let Some(terms) = self.at.session.and_then(|session| sessions.terms(session)) {
            let placeholder = redaction::placeholder(SESSION_TERM_KIND);
            let hits = terms
                .spans(text)
                .into_iter()
                .map(|(range, _)| Hit {
                    range,
                    rule: RuleId::SessionTerm,
                    replacement: placeholder.clone(),
                })
                .collect();
            self.apply(field, text, hits);
        }
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

    fn drop_output(&mut self, field: Field, output: &mut String) {
        let placeholder = redaction::placeholder(DROP_TOOL_OUTPUTS_KIND);
        self.record(field, RuleId::DropToolOutputs, output, &placeholder);
        *output = placeholder;
    }

    fn escape_flags(&mut self, field: &Field, text: &mut String) {
        let literals = super::decide::escapes(text);
        for (range, escaped) in &literals {
            let original = text.get(range.clone()).unwrap_or_default().to_owned();
            self.record(field.clone(), RuleId::FlagLiteral, &original, escaped);
        }
        for (range, escaped) in literals.into_iter().rev() {
            text.replace_range(range, &escaped);
        }
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
        let whole = hits.first().map(|hit| hit.replacement.clone());
        let mut shown: Vec<Shown> = Vec::new();
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
                let at = out.len();
                self.record(field.clone(), hit.rule, original, &hit.replacement);
                out.push_str(&hit.replacement);
                if matches!(hit.rule, RuleId::Injection { .. }) && shown.len() < EXCERPTS_PER_TEXT {
                    shown.push(Shown {
                        change: self.changes.len() - 1,
                        at: at..out.len(),
                        original: piece.clone(),
                    });
                }
                copied = piece.end;
            }
        }
        if copied == 0 {
            return;
        }
        if self.whole
            && let Some(replacement) = whole
        {
            *text = replacement;
            return;
        }
        out.push_str(text.get(copied..).unwrap_or_default());
        for shown in shown {
            let original = text.get(shown.original).unwrap_or_default();
            if let Some(change) = self.changes.get_mut(shown.change) {
                change.excerpt = crate::receive::excerpt(&out, &shown.at, original);
            }
        }
        *text = out;
    }

    fn record(&mut self, field: Field, rule: RuleId, original: &str, replacement: &str) {
        let excerpt = match self.posture {
            Posture::Sanitize => String::new(),
            Posture::Receive => replacement.to_owned(),
        };
        let fingerprint = self.key.map_or_else(String::new, |key| {
            let mut hex = key.hex(&[b"fingerprint", self.digest.as_bytes(), original.as_bytes()]);
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
            excerpt,
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
        let placeholder = self.scrubber.placeholder(redaction::SECRET_KEY_KIND);
        self.scrubber
            .record(field, RuleId::SecretKey, text, &placeholder);
        *text = placeholder;
        true
    }
}

fn overlapping_as_one(hits: Vec<Hit>) -> Vec<Hit> {
    let mut merged: Vec<Hit> = Vec::with_capacity(hits.len());
    for hit in hits {
        match merged.last_mut() {
            Some(last) if hit.range.start < last.range.end => {
                last.range.end = last.range.end.max(hit.range.end);
                if matches!(last.rule, RuleId::Injection { .. })
                    && !matches!(hit.rule, RuleId::Injection { .. })
                {
                    last.rule = hit.rule;
                    last.replacement = hit.replacement;
                }
            }
            _ => merged.push(hit),
        }
    }
    merged
}

fn run_together(hits: Vec<Hit>) -> Vec<Hit> {
    let mut merged: Vec<Hit> = Vec::with_capacity(hits.len());
    for hit in hits {
        match merged.last_mut() {
            Some(last)
                if matches!(hit.rule, RuleId::Injection { .. })
                    && last.rule == hit.rule
                    && hit.range.start == last.range.end =>
            {
                last.range.end = hit.range.end;
            }
            _ => merged.push(hit),
        }
    }
    merged
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

pub(super) fn scan_flags(text: &str) -> bool {
    let mapping = Mapping::default();
    let mut scrubber = Scrubber::new(&mapping, None, "");
    let mut copy = text.to_owned();
    scrubber.text(&Field::Content, &mut copy);
    !scrubber.changes.is_empty()
}

pub(super) fn protected(text: &str, rules: &Ruleset) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut from = 0;
    while let Some(found) = text.get(from..).and_then(|rest| rest.find('[')) {
        let start = from + found;
        let length = text
            .get(start..)
            .map_or(0, |rest| rules.names().placeholder_len(rest));
        if length > 0 {
            ranges.push(start..start + length);
            from = start + length;
        } else {
            from = start + 1;
        }
    }
    ranges
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
