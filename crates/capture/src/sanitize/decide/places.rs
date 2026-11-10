use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ops::Range;

use aho_corasick::AhoCorasick;
use aho_corasick::MatchKind;
use wire::capture::BundledSession;
use wire::capture::FlagPlace;

use super::texts::At;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Span {
    pub range: Range<usize>,
    pub flag: usize,
}

pub(super) type Board = BTreeMap<At, Vec<Span>>;

pub(super) struct Seed {
    pub at: At,
    pub range: Range<usize>,
    pub flag: usize,
}

pub(super) fn locate(
    sessions: &mut [BundledSession],
    seeds: Vec<Seed>,
    texts: &[&str],
) -> Result<Board, aho_corasick::BuildError> {
    let mut board = Board::new();
    for seed in seeds {
        board.entry(seed.at).or_default().push(Span {
            range: seed.range,
            flag: seed.flag,
        });
    }
    let search = AhoCorasick::builder()
        .match_kind(MatchKind::LeftmostLongest)
        .build(texts)?;
    super::texts::visit(sessions, &mut |slot, text| {
        let mut seeded = board.remove(&slot.at).unwrap_or_default();
        seeded.sort_by_key(|span| span.range.start);
        let mut spans = Vec::new();
        let mut from = 0;
        let ends = seeded
            .iter()
            .map(|span| span.range.clone())
            .chain(std::iter::once(text.len()..text.len()));
        for next in ends {
            if let Some(gap) = text.get(from..next.start) {
                spans.extend(search.find_iter(gap).map(|found| Span {
                    range: from + found.start()..from + found.end(),
                    flag: found.pattern().as_usize() + 1,
                }));
            }
            from = next.end;
        }
        spans.extend(seeded);
        if !spans.is_empty() {
            board.insert(slot.at, spans);
        }
    });
    Ok(sorted(board))
}

fn sorted(mut board: Board) -> Board {
    for spans in board.values_mut() {
        spans.sort_by_key(|span| span.range.start);
    }
    board
}

pub(super) fn rewrite(
    text: &mut String,
    spans: &mut [Span],
    mut with: impl FnMut(&Span, Option<&str>) -> Option<String>,
) {
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    for span in spans.iter_mut() {
        let old = if span.range.start >= from {
            text.get(span.range.clone())
        } else {
            None
        };
        let Some(old) = old else {
            with(span, None);
            continue;
        };
        out.push_str(text.get(from..span.range.start).unwrap_or_default());
        let start = out.len();
        match with(span, Some(old)) {
            Some(new) => out.push_str(&new),
            None => out.push_str(old),
        }
        from = span.range.end;
        span.range = start..out.len();
    }
    out.push_str(text.get(from..).unwrap_or_default());
    *text = out;
}

pub(super) fn apply(
    sessions: &mut [BundledSession],
    board: &mut Board,
    mut with: impl FnMut(&Span, Option<&str>) -> Option<String>,
) {
    let mut missing: BTreeSet<At> = board.keys().cloned().collect();
    super::texts::visit(sessions, &mut |slot, text| {
        if let Some(spans) = board.get_mut(&slot.at) {
            missing.remove(&slot.at);
            rewrite(text, spans, &mut with);
        }
    });
    for at in missing {
        for span in board.get(&at).into_iter().flatten() {
            with(span, None);
        }
    }
}

pub(super) fn found(sessions: &mut [BundledSession]) -> Board {
    let mut board = Board::new();
    super::texts::visit(sessions, &mut |slot, text| {
        let spans: Vec<Span> = super::placeholders(text)
            .into_iter()
            .map(|(range, number)| Span {
                range,
                flag: number,
            })
            .collect();
        if !spans.is_empty() {
            board.insert(slot.at, spans);
        }
    });
    board
}

pub(super) fn by_flag(board: &Board) -> BTreeMap<usize, Vec<FlagPlace>> {
    let mut places: BTreeMap<usize, Vec<FlagPlace>> = BTreeMap::new();
    for (at, spans) in board {
        for span in spans {
            places.entry(span.flag).or_default().push(FlagPlace {
                session: at.session,
                message: at.message,
                field: at.field.clone(),
                start: span.range.start,
                end: span.range.end,
            });
        }
    }
    places
}

pub(super) fn board_of(places: impl IntoIterator<Item = (usize, FlagPlace)>) -> Board {
    let mut board = Board::new();
    for (flag, place) in places {
        board
            .entry(At {
                session: place.session,
                message: place.message,
                field: place.field,
            })
            .or_default()
            .push(Span {
                range: place.start..place.end,
                flag,
            });
    }
    sorted(board)
}
