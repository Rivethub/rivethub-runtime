mod chosen;
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
pub use chosen::SessionRules;
pub use decide::Candidates;
pub use decide::Decision;
pub use decide::LocalReview;
pub use decide::ResolveError;
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
    #[error("the flagged texts do not build a search: {0}")]
    Search(#[from] aho_corasick::BuildError),
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error(
        "the report is from the sanitize run at {report}, but the bundle is from the run at {bundle}; resolve with the report written beside this bundle"
    )]
    OtherRun {
        bundle: DateTime<Utc>,
        report: DateTime<Utc>,
    },
    #[error(
        "the bundle stamp says unresolved_flags is {stamped}, but its review table holds {open} open flag(s); the stamp was edited, so ingest refuses it"
    )]
    Unresolved { stamped: u32, open: usize },
    #[error("the local review file does not serialize: {0}")]
    LocalReview(serde_json::Error),
}

#[derive(Debug)]
pub struct Policy {
    pub mapping: Mapping,
    pub key: LocalKey,
    pub sessions: SessionRules,
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
            sessions: self.sessions.text(),
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    sessions: Vec<chosen::SessionText<'a>>,
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
    pub local: Option<LocalReview>,
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
    scrubber.sessions = &policy.sessions;
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
        scores: &decide::Scores,
        review: decide::Review,
    ) -> Result<&Decision, SanitizeError> {
        let decision = decide::flag(&mut self.sessions, candidates, scores, review)?;
        Ok(self.decision.insert(decision))
    }

    fn review(
        &mut self,
        key: &LocalKey,
        at: DateTime<Utc>,
    ) -> Result<Option<LocalReview>, SanitizeError> {
        let Some(decision) = &self.decision else {
            return Ok(None);
        };
        let local = decision
            .local(key, at, (&self.sessions, &self.exported_by))
            .map_err(SanitizeError::LocalReview)?;
        let open = std::collections::BTreeMap::new();
        decide::render(&mut self.sessions, &local, decision.review.mode, &open, key);
        let mut found = recheck(
            &mut self.sessions,
            &mut self.exported_by,
            None,
            &self.raw_digest,
        );
        self.rechecked += found.changes.len();
        self.changes.append(&mut found.changes);
        self.collisions.append(&mut found.collisions);
        Ok(Some(local))
    }

    pub fn seal(mut self, policy: &Policy, at: DateTime<Utc>) -> Result<Sanitized, SanitizeError> {
        let local = self.review(&policy.key, at)?;
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
                review: self
                    .decision
                    .as_ref()
                    .map(|decision| decision.stamp(&policy.key, at)),
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
        Ok(Sanitized {
            sealed,
            report,
            local,
        })
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
    pub total: usize,
    pub applied: Vec<(usize, decide::Resolution)>,
    pub rechecked: Vec<Change>,
}

pub fn resolve(
    bundle: &SessionBundle,
    flags: &ReportFlags,
    local: &LocalReview,
    key: &LocalKey,
) -> Result<Resolved, SanitizeError> {
    let Sanitization::Sanitized(stamp) = &bundle.manifest.sanitization else {
        return Err(SanitizeError::NotSanitized);
    };
    if stamp.at != flags.at {
        return Err(SanitizeError::OtherRun {
            bundle: stamp.at,
            report: flags.at,
        });
    }
    let review = stamp.review.as_ref().ok_or(ResolveError::NoReview)?;
    let marks = decide::marks(
        review,
        &flags.flags,
        local,
        key,
        stamp.at,
        usize::try_from(stamp.unresolved_flags)?,
    )?;
    if local.at != stamp.at {
        return Err(SanitizeError::OtherRun {
            bundle: stamp.at,
            report: local.at,
        });
    }
    if !local
        .holds(&bundle.sessions, &bundle.manifest.exported_by)
        .map_err(SanitizeError::LocalReview)?
    {
        return Err(ResolveError::Moved.into());
    }
    let rendered = |marks| {
        let mut sessions = bundle.sessions.clone();
        let mut exported_by = bundle.manifest.exported_by.clone();
        decide::render(&mut sessions, local, review.mode, marks, key);
        let found = recheck(
            &mut sessions,
            &mut exported_by,
            None,
            &bundle.manifest.bundle_sha256,
        );
        (sessions, exported_by, found.changes)
    };
    let (expected, expected_by, _) = rendered(&marks.before);
    if expected != bundle.sessions || expected_by != bundle.manifest.exported_by {
        return Err(ResolveError::Moved.into());
    }
    let (sessions, exported_by, rechecked) = rendered(&marks.after);
    let restamped = decide::restamped(review, &marks.after);
    let open = restamped
        .flags
        .iter()
        .filter(|flag| flag.mark == wire::capture::FlagMark::Open)
        .count();
    let sealed = settled(crate::bundle::seal(
        exported_by,
        bundle.manifest.exported_at,
        Sanitization::Sanitized(SanitizeStamp {
            unresolved_flags: u32::try_from(open)?,
            review: Some(decide::sealed(key, stamp.at, open, restamped)),
            ..stamp.clone()
        }),
        sessions,
    )?)?;
    Ok(Resolved {
        sealed,
        open,
        total: review.flags.len(),
        applied: marks.applied,
        rechecked,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFlags {
    pub withheld: usize,
    pub unreviewed: usize,
}

pub fn open_flags(bundle: &SessionBundle) -> Result<OpenFlags, SanitizeError> {
    let Sanitization::Sanitized(stamp) = &bundle.manifest.sanitization else {
        return Ok(OpenFlags {
            withheld: 0,
            unreviewed: 0,
        });
    };
    let placed = decide::withheld(&bundle.sessions);
    let Some(review) = &stamp.review else {
        return Ok(OpenFlags {
            withheld: placed,
            unreviewed: usize::try_from(stamp.unresolved_flags)?,
        });
    };
    let open = review
        .flags
        .iter()
        .filter(|flag| flag.mark == wire::capture::FlagMark::Open)
        .count();
    if usize::try_from(stamp.unresolved_flags)? != open {
        return Err(SanitizeError::Unresolved {
            stamped: stamp.unresolved_flags,
            open,
        });
    }
    Ok(match review.mode {
        decide::Mode::Block => OpenFlags {
            withheld: placed.max(open),
            unreviewed: 0,
        },
        decide::Mode::Advisory => OpenFlags {
            withheld: placed,
            unreviewed: open,
        },
    })
}

#[must_use]
pub fn withheld(bundle: &SessionBundle) -> usize {
    match &bundle.manifest.sanitization {
        Sanitization::Raw => 0,
        Sanitization::Sanitized(_) => decide::withheld(&bundle.sessions),
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
