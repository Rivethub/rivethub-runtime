mod chunks;
mod endpoint;
mod local;
mod places;
mod render;
mod resolve;
mod slots;

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::ops::Range;

use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use wire::capture::BundledSession;
use wire::capture::FlagMark;
use wire::capture::FlagStamp;
use wire::capture::ReviewStamp;

pub use chunks::Candidates;
pub use chunks::Chunk;
pub(super) use chunks::candidates;
pub use endpoint::DecideError;
pub use endpoint::Endpoint;
pub use endpoint::Scores;
pub use endpoint::score;
pub use local::BaseField;
pub use local::LocalFlag;
pub use local::LocalReview;
pub use places::Place;
pub use resolve::ResolveError;
pub(super) use resolve::marks;
pub(super) use resolve::render;
pub(super) use resolve::restamped;
pub use slots::BaseValue;
pub use slots::FieldAt;
pub use slots::FieldName;
pub use wire::capture::ReviewMode as Mode;

use super::key::LocalKey;

pub const REDACTED_FLAG: &str = "[REDACTED:flagged]";
const FLAG_OPEN: &str = crate::rules::names::FLAGGED_OPEN;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Review {
    pub top: NonZeroUsize,
    pub threshold: f64,
    pub mode: Mode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub session: String,
    pub owner: String,
    pub field: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Flag {
    pub number: usize,
    pub score: f64,
    pub text: String,
    pub shown: Vec<Shown>,
    places: Vec<Place>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub scored: usize,
    pub calls: usize,
    pub trivial: usize,
    pub model: String,
    pub review: Review,
    pub flags: Vec<Flag>,
    bases: BTreeMap<FieldAt, BaseValue>,
}

impl Flag {
    #[must_use]
    pub fn places(&self) -> &[Place] {
        &self.places
    }
}

impl Decision {
    pub(super) fn stamp(&self, key: &LocalKey, at: DateTime<Utc>) -> ReviewStamp {
        sealed(
            key,
            at,
            self.flags.len(),
            ReviewStamp {
                mode: self.review.mode,
                model: self.model.clone(),
                calls: self.calls,
                flags: self
                    .flags
                    .iter()
                    .map(|flag| FlagStamp {
                        number: flag.number,
                        mark: FlagMark::Open,
                    })
                    .collect(),
                seal: String::new(),
            },
        )
    }

    pub(super) fn local(
        &self,
        key: &LocalKey,
        at: DateTime<Utc>,
        unreviewed: (&[BundledSession], &wire::capture::BundleAuthor),
    ) -> Result<LocalReview, serde_json::Error> {
        LocalReview {
            at,
            unreviewed_sha256: local::digest(unreviewed.0, unreviewed.1)?,
            flags: self
                .flags
                .iter()
                .map(|flag| LocalFlag {
                    number: flag.number,
                    text: flag.text.clone(),
                    places: flag.places.clone(),
                })
                .collect(),
            fields: self
                .bases
                .iter()
                .map(|(at, base)| BaseField {
                    at: at.clone(),
                    base: base.clone(),
                })
                .collect(),
            mac_format: key.format(),
            seal: String::new(),
        }
        .sealed(key)
    }
}

fn framed(bytes: &[u8]) -> Vec<u8> {
    [bytes.len().to_string().as_bytes(), b":", bytes].concat()
}

fn stamp_time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

fn sealed_parts(at: DateTime<Utc>, unresolved: usize, review: &ReviewStamp) -> Vec<Vec<u8>> {
    let mut parts: Vec<String> = vec![
        stamp_time(at),
        unresolved.to_string(),
        review.mode.to_string(),
        review.model.clone(),
        review.calls.to_string(),
    ];
    for flag in &review.flags {
        parts.extend([flag.number.to_string(), flag.mark.to_string()]);
    }
    std::iter::once(b"review".to_vec())
        .chain(parts.iter().map(|part| framed(part.as_bytes())))
        .collect()
}

pub(super) fn sealed(
    key: &LocalKey,
    at: DateTime<Utc>,
    unresolved: usize,
    mut review: ReviewStamp,
) -> ReviewStamp {
    let parts = sealed_parts(at, unresolved, &review);
    let slices: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    review.seal = key.hex(&slices);
    review
}

pub(super) fn holds_stamp_seal(
    key: &LocalKey,
    at: DateTime<Utc>,
    unresolved: usize,
    review: &ReviewStamp,
) -> bool {
    let parts = sealed_parts(at, unresolved, review);
    let slices: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    key.verifies(&slices, &review.seal)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Keep,
    Redact,
    Map { term: String, placeholder: String },
}

impl Resolution {
    #[must_use]
    pub const fn mark(&self) -> FlagMark {
        match self {
            Self::Keep => FlagMark::Keep,
            Self::Redact => FlagMark::Redact,
            Self::Map { .. } => FlagMark::Map,
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[must_use]
pub fn term_spans(text: &str, term: &str) -> Vec<Range<usize>> {
    let (Some(first), Some(last)) = (term.chars().next(), term.chars().next_back()) else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(offset) = text.get(from..).and_then(|rest| rest.find(term)) {
        let start = from + offset;
        let end = start + term.len();
        let before = text.get(..start).and_then(|head| head.chars().next_back());
        let after = text.get(end..).and_then(|tail| tail.chars().next());
        let bounded = (!is_word(first) || !before.is_some_and(is_word))
            && (!is_word(last) || !after.is_some_and(is_word));
        if bounded {
            spans.push(start..end);
            from = end;
        } else {
            from = start + first.len_utf8();
        }
    }
    spans
}

pub const UNICODE_BOUNDARY: &str = r"(?u:\b)";

#[must_use]
pub fn term_regex(term: &str) -> String {
    let edge = |c: Option<char>| {
        if c.is_some_and(is_word) {
            UNICODE_BOUNDARY
        } else {
            ""
        }
    };
    format!(
        "{}{}{}",
        edge(term.chars().next()),
        regex::escape(term),
        edge(term.chars().next_back())
    )
}

struct Ranked {
    score: f64,
    text: String,
}

fn ranked(chunks: &[Chunk], scores: &Scores, review: Review) -> Vec<Ranked> {
    let mut by_session: BTreeMap<usize, Vec<Ranked>> = BTreeMap::new();
    let mut seen: HashSet<(usize, &str)> = HashSet::new();
    for (chunk, &score) in chunks.iter().zip(&scores.scores) {
        if score >= review.threshold && seen.insert((chunk.session, &chunk.text)) {
            by_session.entry(chunk.session).or_default().push(Ranked {
                score,
                text: chunk.text.clone(),
            });
        }
    }
    let mut listed: HashSet<String> = HashSet::new();
    let mut flagged = Vec::new();
    for (_, mut group) in by_session {
        group.sort_by(|a, b| b.score.total_cmp(&a.score));
        group.truncate(review.top.get());
        for entry in group {
            if listed.insert(entry.text.clone()) {
                flagged.push(entry);
            }
        }
    }
    flagged
}

pub(super) fn flag(
    sessions: &mut [BundledSession],
    candidates: Candidates,
    scores: &Scores,
    review: Review,
) -> Result<Decision, aho_corasick::BuildError> {
    let Candidates { chunks, trivial } = candidates;
    let ranked = ranked(&chunks, scores, review);
    let texts: Vec<&str> = ranked.iter().map(|entry| entry.text.as_str()).collect();
    let located = places::locate(sessions, &texts)?;
    let flags: Vec<Flag> = ranked
        .into_iter()
        .zip(located.places)
        .zip(located.shown)
        .filter(|((_, places), _)| !places.is_empty())
        .zip(1..)
        .map(|(((entry, places), shown), number)| Flag {
            number,
            score: entry.score,
            text: entry.text,
            shown,
            places,
        })
        .collect();
    Ok(Decision {
        scored: chunks.len(),
        calls: scores.calls,
        trivial,
        model: scores.model.clone(),
        review,
        flags,
        bases: located.bases,
    })
}

fn placeholder(number: usize) -> String {
    format!("{FLAG_OPEN}{number}]")
}

#[must_use]
pub fn withheld(sessions: &[BundledSession]) -> usize {
    let mut sessions = sessions.to_vec();
    let mut open = HashSet::new();
    slots::fields(&mut sessions, &mut |_, _, mut field| {
        slots::slots(&mut field, &mut |_, text| {
            open.extend(placeholders(text).into_iter().map(|(_, number)| number));
        });
    });
    open.len()
}

pub(super) fn placeholders(text: &str) -> Vec<(Range<usize>, usize)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = text.get(from..).and_then(|rest| rest.find(FLAG_OPEN)) {
        let start = from + offset;
        let digits = start + FLAG_OPEN.len();
        let length = text.get(digits..).map_or(0, |rest| {
            rest.bytes().take_while(u8::is_ascii_digit).count()
        });
        from = digits;
        if length == 0
            || !text
                .get(digits + length..)
                .is_some_and(|rest| rest.starts_with(']'))
        {
            continue;
        }
        if let Some(number) = text.get(digits..digits + length)
            && let Ok(number) = number.parse()
        {
            found.push((start..digits + length + 1, number));
        }
    }
    found
}

pub(super) fn escapes(text: &str) -> Vec<(Range<usize>, String)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = text.get(from..).and_then(|rest| rest.find(FLAG_OPEN)) {
        let start = from + offset;
        let body = start + FLAG_OPEN.len();
        from = body;
        let Some(rest) = text.get(body..) else {
            break;
        };
        let colons = rest.bytes().take_while(|&b| b == b':').count();
        let digits = rest.get(colons..).map_or(0, |tail| {
            tail.bytes().take_while(u8::is_ascii_digit).count()
        });
        let end = body + colons + digits;
        if digits > 0 && text.get(end..).is_some_and(|tail| tail.starts_with(']')) {
            let original = text.get(start..=end).unwrap_or_default();
            let escaped = format!(
                "{FLAG_OPEN}:{}",
                original.get(FLAG_OPEN.len()..).unwrap_or_default()
            );
            found.push((start..end + 1, escaped));
            from = end + 1;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_found_by_number() {
        let text = "a [FLAGGED:12] b [FLAGGED:x] [FLAGGED:3] [FLAGGED::4]";
        let found: Vec<usize> = placeholders(text).into_iter().map(|(_, n)| n).collect();
        assert_eq!(found, vec![12, 3]);
    }

    #[test]
    fn an_escape_adds_one_colon_and_keeps_the_digits() {
        let text = "[FLAGGED:012] [FLAGGED::7] [FLAGGED:] [FLAGGED:x]";
        let escaped: Vec<String> = escapes(text).into_iter().map(|(_, e)| e).collect();
        assert_eq!(escaped, ["[FLAGGED::012]", "[FLAGGED:::7]"]);
    }

    #[test]
    fn a_word_term_matches_whole_words_only() {
        let spans = |text, term| -> Vec<(usize, usize)> {
            term_spans(text, term)
                .into_iter()
                .map(|span| (span.start, span.end))
                .collect()
        };
        assert_eq!(
            spans("Tom, Tomcat and Tom_x met Tom", "Tom"),
            [(0, 3), (26, 29)]
        );
        assert_eq!(spans("Tomás and Tom", "Tom"), [(11, 14)]);
        assert_eq!(term_regex("Tom"), r"(?u:\b)Tom(?u:\b)");
        assert_eq!(spans("a.prod.x b.production", ".prod"), [(1, 6)]);
    }
}
