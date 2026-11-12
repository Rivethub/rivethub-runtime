use std::collections::BTreeMap;
use std::str::FromStr;

use thiserror::Error;
use wire::capture::BundledSession;
use wire::capture::ChecksDecision;
use wire::capture::ReceiptChecks;
use wire::capture::SessionBundle;

use super::Checked;
use super::Family;
use super::tally;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Import,
    Skip,
    HoldSpans,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{0:?} is not a decision; use import, skip or hold-spans")]
pub struct DecisionError(String);

impl FromStr for Decision {
    type Err = DecisionError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "import" => Ok(Self::Import),
            "skip" => Ok(Self::Skip),
            "hold-spans" => Ok(Self::HoldSpans),
            other => Err(DecisionError(other.to_owned())),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Decisions {
    pub all: Option<Decision>,
    pub sessions: BTreeMap<usize, Decision>,
}

#[derive(Debug)]
pub struct Accepted<'a> {
    pub index: usize,
    pub session: &'a BundledSession,
    pub checks: ReceiptChecks,
}

#[derive(Debug)]
pub struct Plan<'a> {
    pub imports: Vec<Accepted<'a>>,
    pub skipped: Vec<usize>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Blocked {
    #[error("the bundle has no session {number}; it holds {count}")]
    NoSuchSession { number: usize, count: usize },
    #[error("the bundle's own author fields carry findings, and --force was not given")]
    Manifest,
    #[error("{} {} no decision yet", sessions_named(.sessions), has(.sessions))]
    Undecided { sessions: Vec<usize> },
    #[error(
        "{} {} secret findings, which only skip or hold-spans may import; pass --force to import them as they are",
        sessions_named(.sessions),
        has(.sessions)
    )]
    SecretImport { sessions: Vec<usize> },
}

fn has(sessions: &[usize]) -> &'static str {
    if sessions.len() == 1 { "has" } else { "have" }
}

fn sessions_named(sessions: &[usize]) -> String {
    let numbers: Vec<String> = sessions
        .iter()
        .map(|index| (index + 1).to_string())
        .collect();
    match numbers.as_slice() {
        [one] => format!("session {one}"),
        _ => format!("sessions {}", numbers.join(", ")),
    }
}

impl Checked {
    pub fn plan<'a>(
        &'a self,
        bundle: &'a SessionBundle,
        decisions: &Decisions,
        force: bool,
    ) -> Result<Plan<'a>, Blocked> {
        let count = bundle.sessions.len();
        if let Some(&index) = decisions.sessions.keys().find(|&&index| index >= count) {
            return Err(Blocked::NoSuchSession {
                number: index + 1,
                count,
            });
        }
        if !force && self.of_manifest().next().is_some() {
            return Err(Blocked::Manifest);
        }
        let rules_sha256 = super::rules_sha256();
        let mut plan = Plan {
            imports: Vec::new(),
            skipped: Vec::new(),
        };
        let (mut undecided, mut secrets) = (Vec::new(), Vec::new());
        for (index, (session, held)) in bundle.sessions.iter().zip(&self.held).enumerate() {
            let found: Vec<_> = self.of_session(index).collect();
            let flagged = !found.is_empty();
            let decision = decisions
                .sessions
                .get(&index)
                .copied()
                .or_else(|| flagged.then_some(decisions.all).flatten());
            let (kept, verdict) = match (flagged, decision) {
                (_, Some(Decision::Skip)) => {
                    plan.skipped.push(index);
                    continue;
                }
                (false, _) => (session, ChecksDecision::Clean),
                (true, None) => {
                    undecided.push(index);
                    continue;
                }
                (true, Some(Decision::Import)) => {
                    if !force && found.iter().any(|finding| finding.family == Family::Secret) {
                        secrets.push(index);
                        continue;
                    }
                    (session, ChecksDecision::Import)
                }
                (true, Some(Decision::HoldSpans)) => (held, ChecksDecision::HoldSpans),
            };
            plan.imports.push(Accepted {
                index,
                session: kept,
                checks: ReceiptChecks {
                    rules_sha256: rules_sha256.clone(),
                    findings_by_family: tally(found.into_iter())
                        .into_iter()
                        .map(|(family, count)| (family.to_string(), count))
                        .collect(),
                    decision: verdict,
                },
            });
        }
        if !undecided.is_empty() {
            return Err(Blocked::Undecided {
                sessions: undecided,
            });
        }
        if !secrets.is_empty() {
            return Err(Blocked::SecretImport { sessions: secrets });
        }
        Ok(plan)
    }
}
