use jiff::Timestamp;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;

use thiserror::Error;
use wire::capture::FlagMark;
use wire::capture::ReviewStamp;

use super::super::marks::MarkState;
use super::super::marks::ReportError;
use super::super::marks::ReportFlag;
use super::super::marks::read_flags;
use super::Decision;
use super::Flag;
use super::LocalReview;
use super::Resolution;
use super::Review;

const SUMMARY: &str = "After the secrets and mapping passes, the decision model ";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RestoreError {
    #[error(transparent)]
    Report(#[from] ReportError),
    #[error("the report has no decision summary that rh can read")]
    Summary,
    #[error("the report or the local review file is from another sanitize run than the bundle")]
    OtherRun,
    #[error("flag {0} is in the bundle's review but not in the local review file")]
    Missing(usize),
    #[error("the report gives flag {0} no score")]
    Score(usize),
    #[error("flag {0} is resolved as map in the bundle, but the report lost its Resolved line")]
    Lost(usize),
}

#[derive(Debug)]
pub struct Restored {
    pub decision: Decision,
    pub resolved: BTreeMap<usize, Resolution>,
}

struct Summary {
    scored: usize,
    trivial: usize,
    top: NonZeroUsize,
    threshold: f64,
}

fn count(text: &str) -> Result<usize, RestoreError> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().map_err(|_| RestoreError::Summary)
}

fn after<'a>(line: &'a str, marker: &str) -> Result<&'a str, RestoreError> {
    line.rsplit_once(marker)
        .map(|(_, rest)| rest)
        .ok_or(RestoreError::Summary)
}

fn summary(report: &str) -> Result<Summary, RestoreError> {
    let line = report
        .lines()
        .find(|line| line.starts_with(SUMMARY))
        .ok_or(RestoreError::Summary)?;
    let threshold = after(line, "at or over ")?
        .split_once(". ")
        .ok_or(RestoreError::Summary)?
        .0;
    Ok(Summary {
        scored: count(after(line, "` scored ")?)?,
        trivial: count(after(line, ". It skipped ")?)?,
        top: NonZeroUsize::new(count(after(line, "up to ")?)?).ok_or(RestoreError::Summary)?,
        threshold: threshold.parse().map_err(|_| RestoreError::Summary)?,
    })
}

pub fn restore(
    review: &ReviewStamp,
    at: Timestamp,
    local: &LocalReview,
    report: &str,
) -> Result<Restored, RestoreError> {
    let written = read_flags(report)?;
    if written.at != at || local.at != at {
        return Err(RestoreError::OtherRun);
    }
    let numbers = summary(report)?;
    let reported: BTreeMap<usize, &ReportFlag> = written
        .flags
        .iter()
        .map(|flag| (flag.number, flag))
        .collect();
    let mut flags = Vec::with_capacity(review.flags.len());
    let mut resolved = BTreeMap::new();
    for stamp in &review.flags {
        let number = stamp.number;
        let kept = local.flag(number).ok_or(RestoreError::Missing(number))?;
        let shown = reported.get(&number);
        flags.push(Flag {
            number,
            score: shown
                .and_then(|flag| flag.score())
                .ok_or(RestoreError::Score(number))?,
            text: kept.text.clone(),
            shown: Vec::new(),
            places: kept.places.clone(),
        });
        let done = match (stamp.mark, shown.map(|flag| &flag.state)) {
            (FlagMark::Open, _) => continue,
            (FlagMark::Keep, _) => Resolution::Keep,
            (FlagMark::Redact, _) => Resolution::Redact,
            (FlagMark::Map, Some(MarkState::Resolved(map @ Resolution::Map { .. }))) => map.clone(),
            (FlagMark::Map, _) => return Err(RestoreError::Lost(number)),
        };
        resolved.insert(number, done);
    }
    Ok(Restored {
        decision: Decision {
            scored: numbers.scored,
            calls: review.calls,
            trivial: numbers.trivial,
            model: review.model.clone(),
            review: Review {
                top: numbers.top,
                threshold: numbers.threshold,
                mode: review.mode,
            },
            flags,
            bases: local.bases(),
        },
        resolved,
    })
}
