use std::collections::BTreeMap;
use std::collections::HashMap;

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
use super::Mode;
use super::Resolution;
use super::placeholder;
use super::places;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResolveError {
    #[error("the bundle has no decision-model review; it was sanitized without --decide")]
    NoReview,
    #[error(
        "the bundle's review table does not match its seal, so it changed after sanitize; sanitize the raw bundle again"
    )]
    Table,
    #[error(
        "flag {0} is in the report but not in the bundle; marks apply only to the bundle's flags"
    )]
    Unknown(usize),
    #[error(
        "flag {0}: the report's hidden copy of the text does not match the bundle's seal; sanitize the raw bundle again"
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
        "flag {0}: the bundle no longer holds the flagged text where sanitize put it; sanitize the raw bundle again"
    )]
    Moved(usize),
}

#[derive(Debug)]
pub(crate) struct Marked {
    pub(crate) review: ReviewStamp,
    pub(crate) applied: Vec<(usize, Resolution)>,
}

impl Marked {
    #[must_use]
    pub(crate) fn open(&self) -> usize {
        self.review
            .flags
            .iter()
            .filter(|flag| flag.mark == FlagMark::Open)
            .count()
    }
}

type Texts = HashMap<usize, String>;

pub(in super::super) fn resolve(
    sessions: &mut [BundledSession],
    review: &ReviewStamp,
    report: &[ReportFlag],
    key: &LocalKey,
    at: DateTime<Utc>,
    recheck: impl FnOnce(&mut [BundledSession]),
) -> Result<Marked, ResolveError> {
    if super::sealed(key, at, review.clone()).seal != review.seal {
        return Err(ResolveError::Table);
    }
    let (mut texts, applied) = checked(review, report, key, at)?;
    let mut board = withdrawn(sessions, review, &mut texts)?;
    let resolved: HashMap<usize, String> = applied
        .iter()
        .filter_map(|(number, resolution)| {
            texts
                .get(number)
                .map(|text| (*number, resolution.apply(text)))
        })
        .collect();
    places::apply(sessions, &mut board, |span, _| {
        resolved.get(&span.flag).cloned()
    });
    recheck(sessions);
    let mut found = relocated(sessions, &board, &resolved)?;
    if review.mode == Mode::Advisory {
        places::apply(sessions, &mut found, |span, _| {
            texts.get(&span.flag).cloned()
        });
    }
    let flags = restamped(review, &applied, &found);
    Ok(Marked {
        review: super::sealed(
            key,
            at,
            ReviewStamp {
                flags,
                seal: String::new(),
                ..review.clone()
            },
        ),
        applied,
    })
}

fn checked(
    review: &ReviewStamp,
    report: &[ReportFlag],
    key: &LocalKey,
    at: DateTime<Utc>,
) -> Result<(Texts, Vec<(usize, Resolution)>), ResolveError> {
    let stamps: BTreeMap<usize, &FlagStamp> = review
        .flags
        .iter()
        .map(|flag| (flag.number, flag))
        .collect();
    let mut texts = Texts::new();
    let mut applied: Vec<(usize, Resolution)> = Vec::new();
    for flag in report {
        let stamp = stamps
            .get(&flag.number)
            .ok_or(ResolveError::Unknown(flag.number))?;
        if super::text_seal(key, at, flag.number, &flag.text) != stamp.seal {
            return Err(ResolveError::Forged(flag.number));
        }
        if !flag.shows_its_text() {
            return Err(ResolveError::Edited(flag.number));
        }
        texts.insert(flag.number, flag.text.clone());
        match (&flag.state, stamp.mark) {
            (MarkState::Open, _) => {}
            (MarkState::Marked(resolution) | MarkState::Resolved(resolution), FlagMark::Open) => {
                applied.push((flag.number, resolution.clone()));
            }
            (MarkState::Marked(resolution) | MarkState::Resolved(resolution), done) => {
                if resolution.mark() != done {
                    return Err(ResolveError::Changed {
                        number: flag.number,
                        done,
                    });
                }
            }
        }
    }
    Ok((texts, applied))
}

fn withdrawn(
    sessions: &mut [BundledSession],
    review: &ReviewStamp,
    texts: &mut Texts,
) -> Result<places::Board, ResolveError> {
    let mut board = places::board_of(review.flags.iter().flat_map(|flag| {
        let open = flag.mark == FlagMark::Open;
        flag.places
            .iter()
            .filter(move |_| open)
            .cloned()
            .map(move |place| (flag.number, place))
    }));
    let mut moved = None;
    places::apply(sessions, &mut board, |span, old| {
        let expected = match (review.mode, old) {
            (Mode::Block, Some(old)) => old == placeholder(span.flag),
            (Mode::Advisory, Some(old)) => {
                texts.entry(span.flag).or_insert_with(|| old.to_owned()) == old
            }
            (_, None) => false,
        };
        if !expected {
            moved.get_or_insert(span.flag);
        }
        Some(placeholder(span.flag))
    });
    match moved {
        Some(number) => Err(ResolveError::Moved(number)),
        None => Ok(board),
    }
}

fn counts(board: &places::Board) -> BTreeMap<usize, usize> {
    let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
    for span in board.values().flatten() {
        *counts.entry(span.flag).or_default() += 1;
    }
    counts
}

fn relocated(
    sessions: &mut [BundledSession],
    board: &places::Board,
    resolved: &HashMap<usize, String>,
) -> Result<places::Board, ResolveError> {
    let found = places::found(sessions);
    let mut expected = counts(board);
    expected.retain(|number, _| !resolved.contains_key(number));
    let now = counts(&found);
    match expected
        .keys()
        .chain(now.keys())
        .find(|number| expected.get(number) != now.get(number))
    {
        Some(number) => Err(ResolveError::Moved(*number)),
        None => Ok(found),
    }
}

fn restamped(
    review: &ReviewStamp,
    applied: &[(usize, Resolution)],
    found: &places::Board,
) -> Vec<FlagStamp> {
    let mut by_flag = places::by_flag(found);
    review
        .flags
        .iter()
        .map(|flag| {
            let mark = applied
                .iter()
                .find(|(number, _)| *number == flag.number)
                .map_or(flag.mark, |(_, resolution)| resolution.mark());
            FlagStamp {
                number: flag.number,
                mark,
                places: if mark == FlagMark::Open {
                    by_flag.remove(&flag.number).unwrap_or_default()
                } else {
                    Vec::new()
                },
                seal: flag.seal.clone(),
            }
        })
        .collect()
}
