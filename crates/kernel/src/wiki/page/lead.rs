use super::bound::{blocks, covered, overlaps, prose, words};
use super::summary::SUMMARY_LIMIT;

pub const CURRENTLY: &str = "Currently";
const CURRENTLY_LIMIT: usize = 300;
const RESTATED_PERCENT: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lead {
    pub lead: String,
    pub currently: Option<String>,
}

#[must_use]
pub fn split_lead(text: &str) -> Lead {
    let text = text.trim();
    let start = text
        .match_indices(CURRENTLY)
        .map(|(at, _)| at)
        .filter(|at| sentence_start(text, *at))
        .last();
    let Some(start) = start else {
        return Lead {
            lead: text.to_owned(),
            currently: None,
        };
    };
    let lead = text.get(..start).unwrap_or_default().trim().to_owned();
    let rest = text.get(start..).unwrap_or_default();
    let end = rest
        .match_indices(". ")
        .map(|(at, _)| at.saturating_add(1))
        .next()
        .unwrap_or(rest.len());
    let sentence: String = rest
        .get(..end)
        .unwrap_or(rest)
        .trim()
        .chars()
        .take(CURRENTLY_LIMIT)
        .collect();
    Lead {
        lead,
        currently: Some(sentence).filter(|sentence| sentence.len() > CURRENTLY.len()),
    }
}

fn sentence_start(text: &str, at: usize) -> bool {
    let before = text.get(..at).unwrap_or_default().trim_end_matches(' ');
    before.is_empty() || before.ends_with(['.', '!', '?', '\n', ':'])
}

#[must_use]
pub fn stable_summary(existing: &str, proposed: &str) -> String {
    let old = split_lead(existing);
    let new = split_lead(proposed);
    let lead = if old.lead.is_empty() {
        one_lead(&new.lead)
    } else {
        one_lead(&old.lead)
    };
    let currently = match (new.currently, old.currently) {
        (Some(new), Some(old)) if restates(&old, &new) => Some(old),
        (new, old) => new.or(old),
    };
    match currently {
        Some(currently) if lead.is_empty() => currently,
        Some(currently) if fits(&lead, &currently) => format!("{lead} {currently}"),
        Some(_) | None => lead,
    }
}

#[must_use]
pub fn steady_lead(existing: &str, proposed: &str) -> String {
    let mut parts = blocks(proposed);
    let old = blocks(existing)
        .into_iter()
        .next()
        .filter(|block| prose(block));
    match (old, parts.first_mut()) {
        (Some(old), Some(new)) if prose(new) => {
            *new = stable_summary(&old, new);
            parts.join("\n\n")
        }
        _ => proposed.trim().to_owned(),
    }
}

fn one_lead(text: &str) -> String {
    let mut paragraphs = blocks(text).into_iter();
    let Some(first) = paragraphs.next() else {
        return String::new();
    };
    let said = words(&first);
    std::iter::once(first)
        .chain(paragraphs.filter(|paragraph| !covered(&words(paragraph), &said)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn restates(old: &str, new: &str) -> bool {
    let before = words(old);
    let after = words(new);
    overlaps(&after, &before, RESTATED_PERCENT)
        && after
            .iter()
            .filter(|word| word.chars().any(|letter| letter.is_ascii_digit()))
            .all(|word| before.contains(word))
}

fn fits(lead: &str, currently: &str) -> bool {
    lead.chars()
        .count()
        .saturating_add(1)
        .saturating_add(currently.chars().count())
        <= SUMMARY_LIMIT
}
