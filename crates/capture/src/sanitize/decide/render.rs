use std::collections::BTreeMap;
use std::ops::Range;

use wire::capture::BundledSession;

use super::super::key::LocalKey;
use super::REDACTED_FLAG;
use super::Resolution;
use super::places::Place;
use super::slots::BaseValue;
use super::slots::FieldAt;
use super::slots::FieldName;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Effect {
    Keep,
    Withhold,
    Apply(Resolution),
}

pub(super) struct Marked<'a> {
    pub number: usize,
    pub places: &'a [Place],
    pub effect: Effect,
}

struct Span<'a> {
    range: Range<usize>,
    number: usize,
    effect: &'a Effect,
}

pub(super) fn apply(
    sessions: &mut [BundledSession],
    bases: &BTreeMap<FieldAt, BaseValue>,
    flags: &[Marked<'_>],
    key: &LocalKey,
) {
    let mut spans: BTreeMap<(&FieldAt, usize), Vec<Span<'_>>> = BTreeMap::new();
    for flag in flags {
        for place in flag.places {
            spans
                .entry((&place.field, place.slot))
                .or_default()
                .push(Span {
                    range: place.start..place.end,
                    number: flag.number,
                    effect: &flag.effect,
                });
        }
    }
    super::slots::fields(sessions, &mut |at, _, mut field| {
        let Some(base) = bases.get(&at) else {
            return;
        };
        field.set(base);
        super::slots::slots(&mut field, &mut |slot, text| {
            if let Some(spans) = spans.get(&(&at, slot.ordinal)) {
                let rendered = render(text, spans);
                if at.name == FieldName::TaskId && rendered != *text {
                    *text = key.uuid(&[b"task_id", b"flagged", text.as_bytes()]);
                } else {
                    *text = rendered;
                }
            }
        });
    });
}

pub(super) fn restore(sessions: &mut [BundledSession], bases: &BTreeMap<FieldAt, BaseValue>) {
    super::slots::fields(sessions, &mut |at, _, mut field| {
        if let Some(base) = bases.get(&at) {
            field.set(base);
        }
    });
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

fn merged(mut ranges: Vec<(Range<usize>, usize)>) -> Vec<(Range<usize>, usize)> {
    ranges.sort_by_key(|(range, _)| range.start);
    let mut out: Vec<(Range<usize>, usize)> = Vec::new();
    for (range, number) in ranges {
        match out.last_mut() {
            Some((last, low)) if range.start <= last.end => {
                last.end = last.end.max(range.end);
                *low = (*low).min(number);
            }
            Some(_) | None => out.push((range, number)),
        }
    }
    out
}

fn without(range: &Range<usize>, cut: &[(Range<usize>, usize)]) -> Vec<Range<usize>> {
    let mut pieces = Vec::new();
    let mut start = range.start;
    for (hole, _) in cut {
        if hole.end <= start || hole.start >= range.end {
            continue;
        }
        if hole.start > start {
            pieces.push(start..hole.start);
        }
        start = start.max(hole.end);
    }
    if start < range.end {
        pieces.push(start..range.end);
    }
    pieces
}

type Hits<'a> = Vec<(Range<usize>, usize, &'a str)>;

fn absorb(
    hits: &mut Hits<'_>,
    redact: &mut Vec<(Range<usize>, usize)>,
    withheld: &mut Vec<(Range<usize>, usize)>,
) {
    loop {
        let before = hits.len();
        hits.retain(|(hit, number, _)| {
            if redact.iter().any(|(range, _)| overlaps(range, hit)) {
                redact.push((hit.clone(), *number));
                return false;
            }
            if let Some(low) = withheld
                .iter()
                .filter(|(range, _)| overlaps(range, hit))
                .map(|(_, low)| *low)
                .min()
            {
                withheld.push((hit.clone(), low));
                return false;
            }
            true
        });
        if hits.len() == before {
            break;
        }
    }
}

fn render(text: &str, spans: &[Span<'_>]) -> String {
    let mut redact: Vec<(Range<usize>, usize)> = Vec::new();
    let mut withheld: Vec<(Range<usize>, usize)> = Vec::new();
    let mut hits: Hits<'_> = Vec::new();
    for span in spans {
        match span.effect {
            Effect::Keep | Effect::Apply(Resolution::Keep) => {}
            Effect::Withhold => withheld.push((span.range.clone(), span.number)),
            Effect::Apply(Resolution::Redact) => redact.push((span.range.clone(), span.number)),
            Effect::Apply(Resolution::Map { term, placeholder }) => {
                let inside = text.get(span.range.clone()).unwrap_or_default();
                hits.extend(super::term_spans(inside, term).into_iter().map(|hit| {
                    (
                        span.range.start + hit.start..span.range.start + hit.end,
                        span.number,
                        placeholder.as_str(),
                    )
                }));
            }
        }
    }
    absorb(&mut hits, &mut redact, &mut withheld);
    let redact = merged(redact);
    let withheld = merged(
        withheld
            .iter()
            .flat_map(|(range, number)| {
                without(range, &redact)
                    .into_iter()
                    .map(move |piece| (piece, *number))
            })
            .collect(),
    );
    hits.sort_by_key(|(range, _, _)| (range.start, usize::MAX - range.len()));
    let mut out: Vec<(Range<usize>, String)> = Vec::new();
    out.extend(
        redact
            .into_iter()
            .map(|(range, _)| (range, REDACTED_FLAG.to_owned())),
    );
    out.extend(
        withheld
            .into_iter()
            .map(|(range, number)| (range, super::placeholder(number))),
    );
    let mut reach = 0;
    for (range, _, placeholder) in hits {
        if range.start >= reach {
            reach = range.end;
            out.push((range, placeholder.to_owned()));
        }
    }
    out.sort_by_key(|(range, _)| range.start);
    let mut rendered = String::with_capacity(text.len());
    let mut from = 0;
    for (range, replacement) in out {
        rendered.push_str(text.get(from..range.start).unwrap_or_default());
        rendered.push_str(&replacement);
        from = range.end;
    }
    rendered.push_str(text.get(from..).unwrap_or_default());
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(term: &str, placeholder: &str) -> Effect {
        Effect::Apply(Resolution::Map {
            term: term.to_owned(),
            placeholder: placeholder.to_owned(),
        })
    }

    fn spans<'a>(list: &'a [(Range<usize>, usize, &'a Effect)]) -> Vec<Span<'a>> {
        list.iter()
            .map(|(range, number, effect)| Span {
                range: range.clone(),
                number: *number,
                effect,
            })
            .collect()
    }

    #[test]
    fn redact_beats_withheld_beats_map_beats_keep() {
        let text = "the Initech ledger moved";
        let redact = Effect::Apply(Resolution::Redact);
        let keep = Effect::Keep;
        let ledger = map("ledger", "L");
        let org = map("Initech", "ORG");
        let withhold = Effect::Withhold;
        for (list, expected) in [
            (
                vec![(0..24, 1, &keep), (4..11, 2, &redact)],
                "the [REDACTED:flagged] ledger moved",
            ),
            (
                vec![(0..24, 1, &ledger), (4..11, 2, &org)],
                "the ORG L moved",
            ),
            (
                vec![(0..24, 1, &redact), (4..11, 2, &org)],
                "[REDACTED:flagged]",
            ),
            (
                vec![(0..24, 1, &withhold), (4..11, 2, &keep)],
                "[FLAGGED:1]",
            ),
            (
                vec![(0..24, 1, &ledger), (4..11, 2, &withhold)],
                "the [FLAGGED:2] L moved",
            ),
            (
                vec![(0..11, 1, &withhold), (4..24, 2, &redact)],
                "[FLAGGED:1][REDACTED:flagged]",
            ),
        ] {
            assert_eq!(render(text, &spans(&list)), expected);
        }
    }

    #[test]
    fn a_map_hit_that_touches_a_withheld_region_joins_it() {
        let text = "Initech ledger";
        let wide = map("Initech ledger", "X");
        let withhold = Effect::Withhold;
        let list = [(0..14, 1, &wide), (8..14, 2, &withhold)];
        assert_eq!(render(text, &spans(&list)), "[FLAGGED:2]");
    }
}
