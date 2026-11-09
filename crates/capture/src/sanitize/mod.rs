pub mod mapping;
pub mod report;

use std::ops::Range;

use chrono::DateTime;
use chrono::Utc;
use serde::Serialize;
use sha2::Digest as _;
use sha2::Sha256;
use strum::IntoEnumIterator as _;
use thiserror::Error;
use wire::JsonObject;
use wire::capture::BundledSession;
use wire::capture::Sanitization;
use wire::capture::SanitizeStamp;
use wire::capture::SessionBundle;
use wire::json::TextAt;
use wire::json::TextSlot;

use crate::bundle::BundleError;
use crate::bundle::Sealed;
use crate::redaction;
use mapping::Action;
use mapping::Matcher;
use mapping::Rule;
pub use report::Change;
pub use report::Field;
pub use report::Pass;
pub use report::Report;
pub use report::RuleId;
pub use report::SessionLine;
pub use report::Spot;

const FINGERPRINT_HEX: usize = 12;

#[derive(Debug, Error)]
pub enum SanitizeError {
    #[error("the bundle is already sanitized (at {at}); sanitize the raw bundle instead")]
    AlreadySanitized { at: DateTime<Utc> },
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error("sanitize policy does not serialize: {0}")]
    Policy(#[from] serde_json::Error),
}

#[derive(Debug, Default)]
pub struct Policy {
    pub rules: Vec<Rule>,
}

impl Policy {
    pub fn sha256(&self) -> Result<String, serde_json::Error> {
        let text = PolicyText {
            secret_keys: redaction::secret_key_source(),
            secret_patterns: redaction::builtins()
                .iter()
                .map(|builtin| (builtin.name, builtin.source))
                .collect(),
            rules: self
                .rules
                .iter()
                .map(|rule| RuleText {
                    matcher: match &rule.matcher {
                        Matcher::Literal(literal) => MatcherText::Literal(literal),
                        Matcher::Regex(regex) => MatcherText::Regex(regex.as_str()),
                    },
                    action: &rule.action,
                })
                .collect(),
        };
        Ok(hex::encode(Sha256::digest(wire::json::canonical_json(
            &text,
        )?)))
    }
}

#[derive(Serialize)]
struct PolicyText<'a> {
    secret_keys: &'static str,
    secret_patterns: Vec<(&'static str, &'static str)>,
    rules: Vec<RuleText<'a>>,
}

#[derive(Serialize)]
struct RuleText<'a> {
    matcher: MatcherText<'a>,
    action: &'a Action,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum MatcherText<'a> {
    Literal(&'a str),
    Regex(&'a str),
}

#[derive(Debug)]
pub struct Sanitized {
    pub sealed: Sealed,
    pub report: Report,
}

pub fn run(
    bundle: &SessionBundle,
    policy: &Policy,
    at: DateTime<Utc>,
) -> Result<Sanitized, SanitizeError> {
    if let Sanitization::Sanitized(stamp) = &bundle.manifest.sanitization {
        return Err(SanitizeError::AlreadySanitized { at: stamp.at });
    }
    let mut scrubber = Scrubber::new(&policy.rules, &bundle.manifest.bundle_sha256);
    let mut sessions = bundle.sessions.clone();
    for (index, session) in sessions.iter_mut().enumerate() {
        scrubber.session(index, session);
    }
    let mut exported_by = bundle.manifest.exported_by.clone();
    scrubber.at = Location::default();
    scrubber.text(&Field::ExporterNode, &mut exported_by.node, Passes::Mapping);
    let policy_sha256 = policy.sha256()?;
    let sealed = crate::bundle::seal(
        exported_by,
        bundle.manifest.exported_at,
        Sanitization::Sanitized(SanitizeStamp {
            at,
            policy_sha256: policy_sha256.clone(),
            passes: Pass::iter().map(|pass| pass.to_string()).collect(),
            unresolved_flags: 0,
        }),
        sessions,
    )?;
    let report = Report {
        at,
        policy_sha256,
        bundle_sha256: sealed.bundle.manifest.bundle_sha256.clone(),
        sessions: sealed.bundle.sessions.iter().map(SessionLine::of).collect(),
        rules: policy
            .rules
            .iter()
            .map(|rule| (rule.line, rule.action.clone()))
            .collect(),
        changes: scrubber.changes,
    };
    Ok(Sanitized { sealed, report })
}

#[must_use]
pub fn secrets(bundle: &SessionBundle) -> Vec<Change> {
    let mut scrubber = Scrubber::new(&[], &bundle.manifest.bundle_sha256);
    for (index, session) in bundle.sessions.iter().enumerate() {
        scrubber.session(index, &mut session.clone());
    }
    scrubber.changes
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Passes {
    All,
    Mapping,
}

#[derive(Debug, Clone, Default)]
struct Location {
    session: Option<usize>,
    event_id: Option<String>,
}

struct Scrubber<'a> {
    rules: &'a [Rule],
    key: &'a str,
    at: Location,
    changes: Vec<Change>,
}

enum Piece {
    Open(String),
    Closed(String),
}

impl<'a> Scrubber<'a> {
    const fn new(rules: &'a [Rule], key: &'a str) -> Self {
        Self {
            rules,
            key,
            at: Location {
                session: None,
                event_id: None,
            },
            changes: Vec::new(),
        }
    }

    fn session(&mut self, index: usize, session: &mut BundledSession) {
        self.at = Location {
            session: Some(index),
            event_id: None,
        };
        self.text(
            &Field::OriginNode,
            &mut session.origin.node,
            Passes::Mapping,
        );
        let batch = &mut session.batch;
        if let Some(title) = &mut batch.title {
            self.text(&Field::Title, title, Passes::All);
        }
        if let Some(settings) = &mut batch.settings {
            self.object(Field::Settings, settings);
        }
        for message in &mut batch.messages {
            self.at.event_id = Some(message.event_id.clone());
            self.text(&Field::Content, &mut message.content, Passes::All);
            if let Some(result) = &mut message.tool_result {
                self.text(&Field::ToolResult, result, Passes::All);
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
        object.visit_text_mut(&mut |at: TextAt<'_>, text: &mut String| match at.slot {
            TextSlot::Key => self.text(
                &field(Spot {
                    path: at.path.to_owned(),
                    key: true,
                }),
                text,
                Passes::All,
            ),
            TextSlot::Value { key } if redaction::secret_key(key) => {
                if text != redaction::SECRET_KEY_PLACEHOLDER {
                    self.record(
                        field(Spot {
                            path: at.path.to_owned(),
                            key: false,
                        }),
                        RuleId::SecretKey,
                        text,
                        redaction::SECRET_KEY_PLACEHOLDER,
                    );
                    redaction::SECRET_KEY_PLACEHOLDER.clone_into(text);
                }
            }
            TextSlot::Value { .. } => {
                self.text(
                    &field(Spot {
                        path: at.path.to_owned(),
                        key: false,
                    }),
                    text,
                    Passes::All,
                );
            }
        });
    }

    fn text(&mut self, field: &Field, text: &mut String, passes: Passes) {
        let mut pieces = protected(text);
        let mut changed = false;
        if passes == Passes::All {
            for builtin in redaction::builtins() {
                changed |= self.apply(
                    &mut pieces,
                    field,
                    |open| builtin.spans(open),
                    &builtin.placeholder(),
                    RuleId::Pattern(builtin.name),
                );
            }
        }
        for rule in self.rules {
            let id = match rule.action {
                Action::Map(_) => RuleId::Mapped { line: rule.line },
                Action::Redact => RuleId::Redacted { line: rule.line },
            };
            changed |= self.apply(
                &mut pieces,
                field,
                |open| rule.matcher.spans(open),
                rule.action.replacement(),
                id,
            );
        }
        if changed {
            *text = pieces
                .into_iter()
                .map(|piece| match piece {
                    Piece::Open(text) | Piece::Closed(text) => text,
                })
                .collect();
        }
    }

    fn apply(
        &mut self,
        pieces: &mut Vec<Piece>,
        field: &Field,
        spans: impl Fn(&str) -> Vec<Range<usize>>,
        replacement: &str,
        rule: RuleId,
    ) -> bool {
        let mut changed = false;
        let mut next = Vec::with_capacity(pieces.len());
        for piece in pieces.drain(..) {
            let open = match piece {
                Piece::Open(open) => open,
                closed @ Piece::Closed(_) => {
                    next.push(closed);
                    continue;
                }
            };
            let found = spans(&open);
            if found.is_empty() {
                next.push(Piece::Open(open));
                continue;
            }
            changed = true;
            let mut copied = 0;
            for span in found {
                if span.start < copied {
                    continue;
                }
                if span.start > copied {
                    next.push(Piece::Open(open[copied..span.start].to_owned()));
                }
                self.record(field.clone(), rule, &open[span.clone()], replacement);
                next.push(Piece::Closed(replacement.to_owned()));
                copied = span.end;
            }
            if copied < open.len() {
                next.push(Piece::Open(open[copied..].to_owned()));
            }
        }
        *pieces = next;
        changed
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
            event_id: self.at.event_id.clone(),
            field,
            rule,
            fingerprint,
            replacement: replacement.to_owned(),
        });
    }
}

const PLACEHOLDER_OPEN: &str = "[REDACTED";

fn protected(text: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut copied = 0;
    let mut from = 0;
    while let Some(found) = text[from..].find(PLACEHOLDER_OPEN) {
        let start = from + found;
        from = start + PLACEHOLDER_OPEN.len();
        let Some(end) = placeholder_end(text, from) else {
            continue;
        };
        if start > copied {
            pieces.push(Piece::Open(text[copied..start].to_owned()));
        }
        pieces.push(Piece::Closed(text[start..end].to_owned()));
        copied = end;
        from = end;
    }
    if copied < text.len() || pieces.is_empty() {
        pieces.push(Piece::Open(text[copied..].to_owned()));
    }
    pieces
}

fn placeholder_end(text: &str, after_open: usize) -> Option<usize> {
    let rest = &text[after_open..];
    if rest.starts_with(']') {
        return Some(after_open + 1);
    }
    let kind = rest.strip_prefix(':')?;
    let length = kind
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .filter(|&length| length > 0 && kind[length..].starts_with(']'))?;
    Some(after_open + 1 + length + 1)
}
