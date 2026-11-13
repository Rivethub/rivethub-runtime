mod hold;
mod report;

use std::collections::BTreeMap;
use std::fmt;

use sha2::Digest as _;
use sha2::Sha256;
use wire::capture::BundledSession;
use wire::capture::SessionBundle;

pub use hold::Accepted;
pub use hold::Blocked;
pub use hold::Decision;
pub use hold::DecisionError;
pub use hold::Decisions;
pub use hold::Plan;
pub use report::ROWS_PER_SESSION;
pub use report::Row;
pub(crate) use report::excerpt;
pub use report::visible;

use crate::rules::injection;
use crate::sanitize::Change;
use crate::sanitize::Field;
use crate::sanitize::RuleId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Family {
    Secret,
    Injection(injection::Family),
}

impl fmt::Display for Family {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Secret => formatter.write_str("secret"),
            Self::Injection(family) => write!(formatter, "{family}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub session: Option<usize>,
    pub message: Option<usize>,
    pub event_id: Option<String>,
    pub field: Field,
    pub rule: &'static str,
    pub family: Family,
    pub excerpt: String,
}

impl Finding {
    fn from_change(change: Change) -> Option<Self> {
        let (rule, family) = match change.rule {
            RuleId::Pattern(id) => (id, Family::Secret),
            RuleId::SecretKey => (crate::redaction::SECRET_KEY_KIND, Family::Secret),
            RuleId::Injection { id, family } => (id, Family::Injection(family)),
            RuleId::Mapped { .. }
            | RuleId::Redacted { .. }
            | RuleId::FlagLiteral
            | RuleId::SessionTerm
            | RuleId::DropToolOutputs => return None,
        };
        Some(Self {
            session: change.session,
            message: change.message,
            event_id: change.event_id,
            field: change.field,
            rule,
            family,
            excerpt: change.excerpt,
        })
    }
}

#[derive(Debug)]
pub struct Checked {
    findings: Vec<Finding>,
    held: Vec<BundledSession>,
}

#[must_use]
pub fn check(bundle: &SessionBundle) -> Checked {
    let crate::sanitize::Held { changes, sessions } = crate::sanitize::held(bundle);
    Checked {
        findings: changes
            .into_iter()
            .filter_map(Finding::from_change)
            .collect(),
        held: sessions,
    }
}

#[must_use]
pub fn rules_sha256() -> String {
    let mut hasher = Sha256::new();
    hasher.update(crate::rules::ruleset().digest().as_bytes());
    hasher.update([0]);
    hasher.update(injection::ruleset().digest().as_bytes());
    hex::encode(hasher.finalize())
}

impl Checked {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    #[must_use]
    pub fn flagged(&self) -> Vec<usize> {
        let mut flagged: Vec<usize> = self
            .findings
            .iter()
            .filter_map(|finding| finding.session)
            .collect();
        flagged.sort_unstable();
        flagged.dedup();
        flagged
    }

    #[must_use]
    pub fn counts(&self) -> BTreeMap<Family, u32> {
        tally(self.findings.iter())
    }

    #[must_use]
    pub fn summary(&self) -> String {
        summarize(&self.counts())
    }

    #[must_use]
    pub fn shown<'a>(
        &'a self,
        bundle: &'a SessionBundle,
        index: usize,
    ) -> Option<&'a BundledSession> {
        if self
            .findings
            .iter()
            .any(|finding| finding.session == Some(index))
        {
            self.held.get(index)
        } else {
            bundle.sessions.get(index)
        }
    }

    pub(crate) fn of_session(&self, index: usize) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(move |finding| finding.session == Some(index))
    }

    pub(crate) fn of_manifest(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.session.is_none())
    }
}

pub(crate) fn tally<'a>(findings: impl Iterator<Item = &'a Finding>) -> BTreeMap<Family, u32> {
    let mut counts = BTreeMap::new();
    for finding in findings {
        *counts.entry(finding.family).or_insert(0) += 1;
    }
    counts
}

#[must_use]
pub fn summarize(counts: &BTreeMap<Family, u32>) -> String {
    counts
        .iter()
        .map(|(family, count)| format!("{family} \u{d7}{count}"))
        .collect::<Vec<_>>()
        .join(", ")
}
