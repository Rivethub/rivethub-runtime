mod chunks;
mod endpoint;
mod places;
mod resolve;
mod texts;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::ops::Range;

use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use wire::capture::BundledSession;
use wire::capture::FlagMark;
use wire::capture::FlagPlace;
use wire::capture::FlagStamp;
use wire::capture::ReviewStamp;

pub use chunks::CHUNK_CHARS;
pub use chunks::Candidates;
pub use chunks::Chunk;
pub(super) use chunks::candidates;
pub use endpoint::ANSWER_CAP;
pub use endpoint::DecideError;
pub use endpoint::Endpoint;
pub use endpoint::QUESTION;
pub use endpoint::Scores;
pub use endpoint::score;
pub use resolve::ResolveError;
pub(super) use resolve::resolve;
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
    places: Vec<FlagPlace>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub scored: usize,
    pub calls: usize,
    pub trivial: usize,
    pub model: String,
    pub review: Review,
    pub flags: Vec<Flag>,
}

impl Decision {
    pub(super) fn stamp(&self, key: &LocalKey, at: DateTime<Utc>) -> ReviewStamp {
        let flags = self
            .flags
            .iter()
            .map(|flag| FlagStamp {
                number: flag.number,
                mark: FlagMark::Open,
                places: flag.places.clone(),
                seal: text_seal(key, at, flag.number, &flag.text),
            })
            .collect();
        sealed(
            key,
            at,
            ReviewStamp {
                mode: self.review.mode,
                model: self.model.clone(),
                calls: self.calls,
                flags,
                seal: String::new(),
            },
        )
    }
}

fn framed(bytes: &[u8]) -> Vec<u8> {
    [bytes.len().to_string().as_bytes(), b":", bytes].concat()
}

fn stamp_time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

pub(super) fn text_seal(key: &LocalKey, at: DateTime<Utc>, number: usize, text: &str) -> String {
    key.hex(&[
        b"flag",
        &framed(stamp_time(at).as_bytes()),
        &framed(number.to_string().as_bytes()),
        &framed(text.as_bytes()),
    ])
}

pub(super) fn sealed(key: &LocalKey, at: DateTime<Utc>, mut review: ReviewStamp) -> ReviewStamp {
    let mut parts: Vec<String> = vec![
        stamp_time(at),
        review.mode.to_string(),
        review.model.clone(),
        review.calls.to_string(),
    ];
    for flag in &review.flags {
        parts.extend([
            flag.number.to_string(),
            flag.mark.to_string(),
            flag.seal.clone(),
            flag.places.len().to_string(),
        ]);
        for place in &flag.places {
            parts.extend([
                place.session.to_string(),
                place
                    .message
                    .map_or_else(|| "-".to_owned(), |message| message.to_string()),
                place.field.clone(),
                place.start.to_string(),
                place.end.to_string(),
            ]);
        }
    }
    let framed: Vec<Vec<u8>> = std::iter::once(b"review".to_vec())
        .chain(parts.iter().map(|part| framed(part.as_bytes())))
        .collect();
    let slices: Vec<&[u8]> = framed.iter().map(Vec::as_slice).collect();
    review.seal = key.hex(&slices);
    review
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Keep,
    Redact,
    Map { term: String, placeholder: String },
}

impl Resolution {
    #[must_use]
    pub fn apply(&self, original: &str) -> String {
        match self {
            Self::Keep => original.to_owned(),
            Self::Redact => REDACTED_FLAG.to_owned(),
            Self::Map { term, placeholder } => {
                let mut out = String::with_capacity(original.len());
                let mut from = 0;
                for range in term_spans(original, term) {
                    out.push_str(original.get(from..range.start).unwrap_or_default());
                    out.push_str(placeholder);
                    from = range.end;
                }
                out.push_str(original.get(from..).unwrap_or_default());
                out
            }
        }
    }

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

#[must_use]
pub fn term_regex(term: &str) -> String {
    let edge = |c: Option<char>| if c.is_some_and(is_word) { r"\b" } else { "" };
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

pub(super) fn flag(
    sessions: &mut [BundledSession],
    candidates: Candidates,
    scores: &Scores,
    review: Review,
) -> Result<Decision, aho_corasick::BuildError> {
    let Candidates { chunks, trivial } = candidates;
    let mut ranked: BTreeMap<usize, Vec<Ranked>> = BTreeMap::new();
    let mut seen: HashSet<(usize, &str)> = HashSet::new();
    for (chunk, &score) in chunks.iter().zip(&scores.scores) {
        if score >= review.threshold && seen.insert((chunk.at.session, &chunk.text)) {
            ranked.entry(chunk.at.session).or_default().push(Ranked {
                score,
                text: chunk.text.clone(),
            });
        }
    }
    let mut numbers: HashMap<String, usize> = HashMap::new();
    let mut flagged: Vec<Ranked> = Vec::new();
    for (_, mut group) in ranked {
        group.sort_by(|a, b| b.score.total_cmp(&a.score));
        group.truncate(review.top.get());
        for entry in group {
            if !numbers.contains_key(&entry.text) {
                numbers.insert(entry.text.clone(), flagged.len() + 1);
                flagged.push(entry);
            }
        }
    }
    let seeds = chunks
        .iter()
        .filter_map(|chunk| {
            numbers.get(&chunk.text).map(|&number| places::Seed {
                at: chunk.at.clone(),
                range: chunk.range.clone(),
                flag: number,
            })
        })
        .collect();
    let texts: Vec<&str> = flagged.iter().map(|entry| entry.text.as_str()).collect();
    let mut board = places::locate(sessions, seeds, &texts)?;
    if review.mode == Mode::Block {
        places::apply(sessions, &mut board, |span, _| Some(placeholder(span.flag)));
    }
    let mut by_flag = places::by_flag(&board);
    let flags = flagged
        .into_iter()
        .zip(1..)
        .map(|(entry, number)| {
            let places = by_flag.remove(&number).unwrap_or_default();
            Flag {
                number,
                score: entry.score,
                shown: places.iter().map(|place| shown(sessions, place)).collect(),
                text: entry.text,
                places,
            }
        })
        .collect();
    Ok(Decision {
        scored: chunks.len(),
        calls: scores.calls,
        trivial,
        model: scores.model.clone(),
        review,
        flags,
    })
}

fn shown(sessions: &[BundledSession], place: &FlagPlace) -> Shown {
    let session = sessions.get(place.session);
    let key = session.map(|session| session.origin.session_key.clone());
    let owner = match place.message {
        Some(at) => session
            .and_then(|session| session.batch.messages.get(at))
            .map(|message| message.event_id.clone()),
        None => key.clone(),
    };
    Shown {
        session: key.unwrap_or_default(),
        owner: owner.unwrap_or_default(),
        field: place.field.clone(),
    }
}

fn placeholder(number: usize) -> String {
    format!("{FLAG_OPEN}{number}]")
}

#[must_use]
pub fn withheld(sessions: &[BundledSession]) -> usize {
    let mut sessions = sessions.to_vec();
    let mut open = HashSet::new();
    texts::visit(&mut sessions, &mut |_, text| {
        open.extend(placeholders(text).into_iter().map(|(_, number)| number));
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
        let map = Resolution::Map {
            term: "Tom".to_owned(),
            placeholder: "P".to_owned(),
        };
        assert_eq!(
            map.apply("Tom, Tomcat and Tom_x met Tom"),
            "P, Tomcat and Tom_x met P"
        );
        assert_eq!(term_regex("Tom"), r"\bTom\b");
        let dotted = Resolution::Map {
            term: ".prod".to_owned(),
            placeholder: ".P".to_owned(),
        };
        assert_eq!(dotted.apply("a.prod.x b.production"), "a.P.x b.production");
    }
}
