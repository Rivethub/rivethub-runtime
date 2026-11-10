pub mod decide;
pub mod key;
pub mod mapping;
pub mod marks;
pub mod report;
mod rescan;
mod scrub;

use chrono::DateTime;
use chrono::Utc;
use serde::Serialize;
use strum::IntoEnumIterator as _;
use thiserror::Error;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::Sanitization;
use wire::capture::SanitizeStamp;
use wire::capture::SessionBundle;

use crate::bundle::BundleError;
use crate::bundle::Sealed;
pub use decide::Candidates;
pub use decide::Decision;
pub use key::KeyError;
pub use key::LocalKey;
use mapping::Action;
use mapping::Mapping;
use mapping::Matcher;
pub use marks::MarkState;
pub use marks::ReportFlags;
pub use report::Change;
pub use report::Collision;
pub use report::Field;
pub use report::Pass;
pub use report::Report;
pub use report::RuleId;
pub use report::SessionLine;
pub use report::Spot;
use rescan::Found;
use rescan::Rescan;
use scrub::Scrubber;

const RECHECKS: usize = 4;

#[derive(Debug, Error)]
pub enum SanitizeError {
    #[error("the bundle is already sanitized (at {at}); sanitize the raw bundle instead")]
    AlreadySanitized { at: DateTime<Utc> },
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error("sanitize policy does not serialize: {0}")]
    Policy(#[from] serde_json::Error),
    #[error(
        "the sanitized bundle still holds {0} secret pattern match(es) after every re-check; this is a sanitizer bug, so nothing was written"
    )]
    Unsettled(usize),
    #[error("the flag count does not fit the bundle stamp: {0}")]
    FlagCount(#[from] std::num::TryFromIntError),
    #[error("the bundle is not sanitized, so it has no flags to resolve")]
    NotSanitized,
    #[error(
        "the report is from the sanitize run at {report}, but the bundle is from the run at {bundle}; resolve with the report written beside this bundle"
    )]
    OtherRun {
        bundle: DateTime<Utc>,
        report: DateTime<Utc>,
    },
}

#[derive(Debug)]
pub struct Policy {
    pub mapping: Mapping,
    pub key: LocalKey,
}

impl Policy {
    pub fn hash(&self, raw_bundle_sha256: &str) -> Result<String, serde_json::Error> {
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
        Ok(self.key.hex(&[
            raw_bundle_sha256.as_bytes(),
            &wire::json::canonical_json(&text)?,
        ]))
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

#[derive(Debug)]
pub struct Scrubbed {
    raw_digest: String,
    exported_by: BundleAuthor,
    exported_at: DateTime<Utc>,
    sessions: Vec<BundledSession>,
    changes: Vec<Change>,
    collisions: Vec<Collision>,
    rechecked: usize,
    decision: Option<Decision>,
}

pub fn run(
    bundle: &SessionBundle,
    policy: &Policy,
    at: DateTime<Utc>,
) -> Result<Sanitized, SanitizeError> {
    scrub(bundle, policy)?.seal(policy, at)
}

pub fn scrub(bundle: &SessionBundle, policy: &Policy) -> Result<Scrubbed, SanitizeError> {
    if let Sanitization::Sanitized(stamp) = &bundle.manifest.sanitization {
        return Err(SanitizeError::AlreadySanitized { at: stamp.at });
    }
    let raw_digest = &bundle.manifest.bundle_sha256;
    let mut scrubber = Scrubber::new(&policy.mapping, Some(&policy.key), raw_digest);
    scrubber.escape_flags = true;
    let mut sessions = bundle.sessions.clone();
    for (index, session) in sessions.iter_mut().enumerate() {
        let ids = event_ids(&policy.key, session);
        session.origin.identity = Some(policy.key.hex(&[
            b"conversation",
            session.origin.node.as_bytes(),
            session.origin.conversation_id.as_bytes(),
        ]));
        scrubber.session(index, session);
        for (message, id) in session.batch.messages.iter_mut().zip(&ids) {
            message.event_id.clone_from(id);
        }
        scrubber.name_events(index, &ids);
    }
    let mut exported_by = bundle.manifest.exported_by.clone();
    scrubber.manifest(&mut exported_by);
    let mut found = recheck(
        &mut sessions,
        &mut exported_by,
        Some(&policy.key),
        raw_digest,
    );
    let rechecked = found.changes.len();
    scrubber.changes.append(&mut found.changes);
    scrubber.collisions.append(&mut found.collisions);
    Ok(Scrubbed {
        raw_digest: raw_digest.clone(),
        exported_by,
        exported_at: bundle.manifest.exported_at,
        sessions,
        changes: scrubber.changes,
        collisions: scrubber.collisions,
        rechecked,
        decision: None,
    })
}

impl Scrubbed {
    pub fn candidates(&mut self) -> Candidates {
        decide::candidates(&mut self.sessions)
    }

    pub fn flag(
        &mut self,
        candidates: Candidates,
        scores: &[f64],
        review: decide::Review,
    ) -> &Decision {
        self.decision
            .insert(decide::flag(&mut self.sessions, candidates, scores, review))
    }

    pub fn seal(self, policy: &Policy, at: DateTime<Utc>) -> Result<Sanitized, SanitizeError> {
        let policy_sha256 = policy.hash(&self.raw_digest)?;
        let unresolved_flags = self
            .decision
            .as_ref()
            .map_or(0, |decision| decision.flags.len());
        let sealed = settled(crate::bundle::seal(
            self.exported_by,
            self.exported_at,
            Sanitization::Sanitized(SanitizeStamp {
                at,
                policy_sha256: policy_sha256.clone(),
                passes: Pass::iter()
                    .filter(|pass| *pass != Pass::Decide || self.decision.is_some())
                    .map(|pass| pass.to_string())
                    .collect(),
                unresolved_flags: u32::try_from(unresolved_flags)?,
            }),
            self.sessions,
        )?)?;
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
            changes: self.changes,
            collisions: self.collisions,
            rechecked: self.rechecked,
            decision: self.decision,
        };
        Ok(Sanitized { sealed, report })
    }
}

fn settled(sealed: Sealed) -> Result<Sealed, SanitizeError> {
    let remaining = secrets(&sealed.bundle).len();
    if remaining > 0 {
        return Err(SanitizeError::Unsettled(remaining));
    }
    Ok(sealed)
}

#[derive(Debug)]
pub struct Resolved {
    pub sealed: Sealed,
    pub open: usize,
    pub rechecked: Vec<Change>,
}

pub fn resolve(bundle: &SessionBundle, flags: &ReportFlags) -> Result<Resolved, SanitizeError> {
    let Sanitization::Sanitized(stamp) = &bundle.manifest.sanitization else {
        return Err(SanitizeError::NotSanitized);
    };
    if stamp.at != flags.at {
        return Err(SanitizeError::OtherRun {
            bundle: stamp.at,
            report: flags.at,
        });
    }
    let marks: Vec<decide::Marked<'_>> = flags
        .flags
        .iter()
        .filter_map(|flag| match &flag.state {
            MarkState::Marked(resolution) | MarkState::Resolved(resolution) => {
                Some(decide::Marked {
                    number: flag.number,
                    resolution,
                    original: &flag.original,
                    places: &flag.places,
                })
            }
            MarkState::Open => None,
        })
        .collect();
    let mut sessions = bundle.sessions.clone();
    decide::resolve(&mut sessions, &marks);
    let mut exported_by = bundle.manifest.exported_by.clone();
    let rechecked = recheck(
        &mut sessions,
        &mut exported_by,
        None,
        &bundle.manifest.bundle_sha256,
    )
    .changes;
    let unmarked = flags
        .flags
        .iter()
        .filter(|flag| flag.state == MarkState::Open)
        .count();
    let open = unmarked.max(decide::withheld(&sessions));
    let sealed = settled(crate::bundle::seal(
        exported_by,
        bundle.manifest.exported_at,
        Sanitization::Sanitized(SanitizeStamp {
            unresolved_flags: u32::try_from(open)?,
            ..stamp.clone()
        }),
        sessions,
    )?)?;
    Ok(Resolved {
        sealed,
        open,
        rechecked,
    })
}

#[must_use]
pub fn withheld(bundle: &SessionBundle) -> usize {
    match &bundle.manifest.sanitization {
        Sanitization::Raw => 0,
        Sanitization::Sanitized(_) => decide::withheld(&bundle.sessions),
    }
}

pub fn unreviewed(bundle: &SessionBundle) -> Result<usize, SanitizeError> {
    match &bundle.manifest.sanitization {
        Sanitization::Raw => Ok(0),
        Sanitization::Sanitized(stamp) => Ok(usize::try_from(stamp.unresolved_flags)?),
    }
}

fn recheck(
    sessions: &mut [BundledSession],
    exported_by: &mut BundleAuthor,
    key: Option<&LocalKey>,
    digest: &str,
) -> Found {
    let mut all = Found::default();
    for round in 0..=RECHECKS {
        let mut found = Rescan {
            key,
            digest,
            whole: round == RECHECKS,
        }
        .run(sessions, exported_by);
        if found.changes.is_empty() {
            break;
        }
        all.changes.append(&mut found.changes);
        all.collisions.append(&mut found.collisions);
    }
    all
}

#[must_use]
pub fn secrets(bundle: &SessionBundle) -> Vec<Change> {
    let mut sessions = bundle.sessions.clone();
    let mut exported_by = bundle.manifest.exported_by.clone();
    Rescan {
        key: None,
        digest: &bundle.manifest.bundle_sha256,
        whole: false,
    }
    .run(&mut sessions, &mut exported_by)
    .changes
}

fn event_ids(key: &LocalKey, session: &BundledSession) -> Vec<String> {
    session
        .batch
        .messages
        .iter()
        .map(|message| {
            key.hex(&[
                b"event_id",
                session.origin.node.as_bytes(),
                session.origin.conversation_id.as_bytes(),
                session.batch.session_key.as_bytes(),
                message.event_id.as_bytes(),
            ])
        })
        .collect()
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
