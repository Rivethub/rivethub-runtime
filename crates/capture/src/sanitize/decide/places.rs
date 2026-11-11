use std::collections::BTreeMap;
use std::ops::Range;

use aho_corasick::AhoCorasick;
use serde::Deserialize;
use serde::Serialize;
use wire::capture::BundledSession;

use super::Shown;
use super::slots::BaseValue;
use super::slots::FieldAt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    pub field: FieldAt,
    pub slot: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Default)]
pub(super) struct Located {
    pub places: Vec<Vec<Place>>,
    pub shown: Vec<Vec<Shown>>,
    pub bases: BTreeMap<FieldAt, BaseValue>,
}

pub(super) fn locate(
    sessions: &mut [BundledSession],
    texts: &[&str],
) -> Result<Located, aho_corasick::BuildError> {
    let search = AhoCorasick::new(texts)?;
    let mut located = Located {
        places: vec![Vec::new(); texts.len()],
        shown: vec![Vec::new(); texts.len()],
        bases: BTreeMap::new(),
    };
    super::slots::fields(sessions, &mut |at, owner, mut field| {
        let mut hit = false;
        super::slots::slots(&mut field, &mut |slot, text| {
            let held = held(text);
            for found in search.find_overlapping_iter(text.as_str()) {
                let range = found.start()..found.end();
                if held.iter().any(|held| cuts(&range, held)) {
                    continue;
                }
                let flag = found.pattern().as_usize();
                hit = true;
                if let Some(places) = located.places.get_mut(flag) {
                    places.push(Place {
                        field: at.clone(),
                        slot: slot.ordinal,
                        start: range.start,
                        end: range.end,
                    });
                }
                if let Some(shown) = located.shown.get_mut(flag) {
                    let place = Shown {
                        session: owner.session.to_owned(),
                        owner: owner.id.to_owned(),
                        field: slot.shown(at.name),
                    };
                    if !shown.contains(&place) {
                        shown.push(place);
                    }
                }
            }
        });
        if hit {
            located.bases.insert(at, field.base());
        }
    });
    Ok(located)
}

fn held(text: &str) -> Vec<Range<usize>> {
    let mut held = super::super::scrub::protected(text, crate::rules::ruleset());
    held.extend(super::escapes(text).into_iter().map(|(range, _)| range));
    held
}

fn cuts(range: &Range<usize>, held: &Range<usize>) -> bool {
    range.start < held.end
        && held.start < range.end
        && !(range.start <= held.start && held.end <= range.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_match_may_hold_a_placeholder_but_never_cut_one() {
        let held = 4..12;
        assert!(cuts(&(2..6), &held));
        assert!(cuts(&(6..8), &held));
        assert!(cuts(&(10..14), &held));
        assert!(!cuts(&(2..14), &held));
        assert!(!cuts(&(4..12), &held));
        assert!(!cuts(&(0..4), &held));
        assert!(!cuts(&(12..20), &held));
    }
}
