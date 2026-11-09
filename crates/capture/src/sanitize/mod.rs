pub mod mapping;
pub mod report;
mod scrub;

use chrono::DateTime;
use chrono::Utc;
use serde::Serialize;
use sha2::Digest as _;
use sha2::Sha256;
use strum::IntoEnumIterator as _;
use thiserror::Error;
use wire::capture::CaptureMessage;
use wire::capture::Sanitization;
use wire::capture::SanitizeStamp;
use wire::capture::SessionBundle;

use crate::bundle::BundleError;
use crate::bundle::Sealed;
use mapping::Action;
use mapping::Mapping;
use mapping::Matcher;
pub use report::Change;
pub use report::Collision;
pub use report::Field;
pub use report::Pass;
pub use report::Report;
pub use report::RuleId;
pub use report::SessionLine;
pub use report::Spot;
use scrub::Scrubber;

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
    pub mapping: Mapping,
}

impl Policy {
    pub fn sha256(&self, raw_bundle_sha256: &str) -> Result<String, serde_json::Error> {
        let text = PolicyText {
            rules: crate::rules::ruleset().digest(),
            mapping: self
                .mapping
                .rules()
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
        let mut hasher = Sha256::new();
        hasher.update(raw_bundle_sha256.as_bytes());
        hasher.update([0]);
        hasher.update(wire::json::canonical_json(&text)?);
        Ok(hex::encode(hasher.finalize()))
    }
}

#[derive(Serialize)]
struct PolicyText<'a> {
    rules: &'a str,
    mapping: Vec<RuleText<'a>>,
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
    let raw_digest = &bundle.manifest.bundle_sha256;
    let mut scrubber = Scrubber::new(&policy.mapping, raw_digest);
    let mut sessions = bundle.sessions.clone();
    for (index, session) in sessions.iter_mut().enumerate() {
        scrubber.session(index, session);
        let ids: Vec<String> = session
            .batch
            .messages
            .iter()
            .enumerate()
            .map(|(position, message)| event_id(position, message))
            .collect::<Result<_, _>>()?;
        for (message, id) in session.batch.messages.iter_mut().zip(&ids) {
            message.event_id.clone_from(id);
        }
        scrubber.name_events(index, &ids);
    }
    let mut exported_by = bundle.manifest.exported_by.clone();
    scrubber.at = scrub::Location::default();
    scrubber.text(&Field::ExporterNode, &mut exported_by.node);
    let policy_sha256 = policy.sha256(raw_digest)?;
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
            .mapping
            .rules()
            .iter()
            .map(|rule| (rule.line, rule.action.clone()))
            .collect(),
        changes: scrubber.changes,
        collisions: scrubber.collisions,
    };
    Ok(Sanitized { sealed, report })
}

#[must_use]
pub fn secrets(bundle: &SessionBundle) -> Vec<Change> {
    let mapping = Mapping::default();
    let mut scrubber = Scrubber::new(&mapping, &bundle.manifest.bundle_sha256);
    for (index, session) in bundle.sessions.iter().enumerate() {
        let mut copy = session.clone();
        scrubber.session(index, &mut copy);
        let ids: Vec<String> = session
            .batch
            .messages
            .iter()
            .map(|message| message.event_id.clone())
            .collect();
        scrubber.name_events(index, &ids);
    }
    scrubber.changes
}

fn event_id(position: usize, message: &CaptureMessage) -> Result<String, serde_json::Error> {
    let mut unnamed = message.clone();
    unnamed.event_id = String::new();
    let mut hasher = Sha256::new();
    hasher.update(position.to_string().as_bytes());
    hasher.update([0]);
    hasher.update(wire::json::canonical_json(&unnamed)?);
    Ok(hex::encode(hasher.finalize()))
}

impl Scrubber<'_> {
    fn name_events(&mut self, session: usize, ids: &[String]) {
        for change in &mut self.changes {
            if change.session == Some(session)
                && let Some(position) = change.message
            {
                change.event_id = ids.get(position).cloned();
            }
        }
        for collision in &mut self.collisions {
            if collision.session == Some(session)
                && let Some(position) = collision.message
            {
                collision.event_id = ids.get(position).cloned();
            }
        }
    }
}
