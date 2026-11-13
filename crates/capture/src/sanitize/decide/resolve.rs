use std::collections::BTreeMap;

use chrono::DateTime;
use chrono::Utc;
use thiserror::Error;
use wire::capture::BundledSession;
use wire::capture::FlagMark;
use wire::capture::FlagStamp;
use wire::capture::ReviewStamp;

use super::super::key::LocalKey;
use super::super::marks::MarkState;
use super::super::marks::ReportFlag;
use super::LocalReview;
use super::Mode;
use super::Resolution;
use super::render::Effect;
use super::render::Marked;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResolveError {
    #[error("the bundle has no decision-model review; it was sanitized without --decide")]
    NoReview,
    #[error(
        "the local review file was sealed under another machine's sanitize key, or edited; resolve on the machine that ran sanitize, with the files it wrote"
    )]
    OtherKey,
    #[error(
        "the bundle's review table does not match its seal, so it changed after sanitize; sanitize the raw bundle again"
    )]
    Table,
    #[error(
        "flag {0} is in the report but not in the bundle; marks apply only to the bundle's flags"
    )]
    Unknown(usize),
    #[error(
        "flag {0}: the report's hidden copy of the text does not match the local review file; sanitize the raw bundle again"
    )]
    Forged(usize),
    #[error(
        "flag {0}: the text under the flag was edited; put it back as sanitize wrote it and change text with `map <term> as <placeholder>`"
    )]
    Edited(usize),
    #[error(
        "flag {number} is already resolved as {done}; sanitize the raw bundle again to change it"
    )]
    Changed { number: usize, done: FlagMark },
    #[error(
        "flag {0} is resolved as map, but the report lost its Resolved line; put the flag's section back as resolve wrote it"
    )]
    Lost(usize),
    #[error(
        "the bundle changed after sanitize or the last resolve (or a Resolved line in the report did); sanitize the raw bundle again"
    )]
    Moved,
}

#[derive(Debug)]
pub(crate) struct Marks {
    pub(crate) before: BTreeMap<usize, Option<Resolution>>,
    pub(crate) after: BTreeMap<usize, Option<Resolution>>,
    pub(crate) applied: Vec<(usize, Resolution)>,
}

pub(in super::super) fn marks(
    review: &ReviewStamp,
    report: &[ReportFlag],
    local: &LocalReview,
    key: &LocalKey,
    at: DateTime<Utc>,
    unresolved: usize,
) -> Result<Marks, ResolveError> {
    if !local.holds_its_seal(key) {
        return Err(ResolveError::OtherKey);
    }
    if !super::holds_stamp_seal(key, at, unresolved, review) {
        return Err(ResolveError::Table);
    }
    let reported: BTreeMap<usize, &ReportFlag> =
        report.iter().map(|flag| (flag.number, flag)).collect();
    if let Some(number) = reported
        .keys()
        .find(|number| !review.flags.iter().any(|flag| flag.number == **number))
    {
        return Err(ResolveError::Unknown(*number));
    }
    let mut marks = Marks {
        before: BTreeMap::new(),
        after: BTreeMap::new(),
        applied: Vec::new(),
    };
    for stamp in &review.flags {
        let number = stamp.number;
        let flag = reported.get(&number);
        if let Some(flag) = flag {
            let text = local.flag(number).map(|local| local.text.as_str());
            if text != Some(flag.text.as_str()) {
                return Err(ResolveError::Forged(number));
            }
            if !flag.shows_its_text() {
                return Err(ResolveError::Edited(number));
            }
        }
        let written = flag.and_then(|flag| match &flag.state {
            MarkState::Open => None,
            MarkState::Marked(resolution) | MarkState::Resolved(resolution) => Some(resolution),
        });
        let (before, after) = match (stamp.mark, written) {
            (FlagMark::Open, None) => (None, None),
            (FlagMark::Open, Some(resolution)) => {
                marks.applied.push((number, resolution.clone()));
                (None, Some(resolution.clone()))
            }
            (done, Some(resolution)) if resolution.mark() == done => {
                (Some(resolution.clone()), Some(resolution.clone()))
            }
            (done, Some(_)) => return Err(ResolveError::Changed { number, done }),
            (FlagMark::Keep, None) => (Some(Resolution::Keep), Some(Resolution::Keep)),
            (FlagMark::Redact, None) => (Some(Resolution::Redact), Some(Resolution::Redact)),
            (FlagMark::Map, None) => return Err(ResolveError::Lost(number)),
        };
        marks.before.insert(number, before);
        marks.after.insert(number, after);
    }
    Ok(marks)
}

pub(in super::super) fn render(
    sessions: &mut [BundledSession],
    local: &LocalReview,
    mode: Mode,
    marks: &BTreeMap<usize, Option<Resolution>>,
    key: &LocalKey,
) {
    let flags: Vec<Marked<'_>> = local
        .flags
        .iter()
        .map(|flag| Marked {
            number: flag.number,
            places: &flag.places,
            effect: match (marks.get(&flag.number).cloned().flatten(), mode) {
                (Some(resolution), _) => Effect::Apply(resolution),
                (None, Mode::Block) => Effect::Withhold,
                (None, Mode::Advisory) => Effect::Keep,
            },
        })
        .collect();
    super::render::apply(sessions, &local.bases(), &flags, key);
}

pub(in super::super) fn restamped(
    review: &ReviewStamp,
    marks: &BTreeMap<usize, Option<Resolution>>,
) -> ReviewStamp {
    ReviewStamp {
        flags: review
            .flags
            .iter()
            .map(|flag| FlagStamp {
                number: flag.number,
                mark: marks
                    .get(&flag.number)
                    .cloned()
                    .flatten()
                    .map_or(FlagMark::Open, |resolution| resolution.mark()),
            })
            .collect(),
        seal: String::new(),
        ..review.clone()
    }
}
