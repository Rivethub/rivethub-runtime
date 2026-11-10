use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::time::Duration;

use futures::StreamExt as _;
use futures::TryStreamExt as _;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
use wire::capture::BundledSession;
use wire::json::Scrub;

use super::report::Field;
use super::report::Spot;

pub const QUESTION: &str = "This line comes from an AI coding session that will be shared outside the author's household. Should it be redacted before sharing?";
pub const CHUNK_CHARS: usize = 1200;
pub const REDACTED_FLAG: &str = "[REDACTED:flagged]";
const FLAG_OPEN: &str = "[FLAGGED:";
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub url: String,
    pub concurrency: NonZeroUsize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Review {
    pub top: NonZeroUsize,
    pub threshold: f64,
    pub mode: Mode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum Mode {
    Advisory,
    Block,
}

#[derive(Debug, Error)]
pub enum DecideError {
    #[error("the HTTP client for the decision model could not start: {0}")]
    Client(reqwest::Error),
    #[error("the decision model at {url} could not be reached: {source}")]
    Unreachable { url: String, source: reqwest::Error },
    #[error("the decision model at {url} answered HTTP {status}: {body}")]
    Status {
        url: String,
        status: u16,
        body: String,
    },
    #[error("the decision model at {url} sent an answer without two probabilities: {detail}")]
    Answer { url: String, detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub id: String,
    pub text: String,
    session: usize,
    slot: usize,
    range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidates {
    pub chunks: Vec<Chunk>,
    pub trivial: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Flag {
    pub number: usize,
    pub session: String,
    pub places: Vec<String>,
    pub score: f64,
    pub original: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub scored: usize,
    pub calls: usize,
    pub trivial: usize,
    pub review: Review,
    pub flags: Vec<Flag>,
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
            Self::Map { term, placeholder } => original.replace(term.as_str(), placeholder),
        }
    }
}

pub(super) fn candidates(sessions: &mut [BundledSession]) -> Candidates {
    let mut chunks = Vec::new();
    let mut trivial = 0;
    let mut slot = 0;
    visit_texts(sessions, &mut |session, owner, field, text| {
        for (index, range) in split(text).into_iter().enumerate() {
            let piece = &text[range.clone()];
            if is_trivial(piece) {
                trivial += 1;
                continue;
            }
            chunks.push(Chunk {
                id: format!("{owner}:{field}:{index}"),
                text: piece.to_owned(),
                session,
                slot,
                range,
            });
        }
        slot += 1;
    });
    Candidates { chunks, trivial }
}

fn is_trivial(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '{' | '}'))
}

fn split(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut chars = 0;
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        let length = line.chars().count();
        if chars > 0 && chars + length > CHUNK_CHARS {
            ranges.push(start..end);
            start = end;
            chars = 0;
        }
        if length > CHUNK_CHARS {
            let mut cut = end;
            for (count, (offset, _)) in line.char_indices().enumerate() {
                if count > 0 && count % CHUNK_CHARS == 0 {
                    ranges.push(cut..end + offset);
                    cut = end + offset;
                }
            }
            start = cut;
            chars = line[cut - end..].chars().count();
        } else {
            chars += length;
        }
        end += line.len();
    }
    if chars > 0 {
        ranges.push(start..end);
    }
    ranges
        .into_iter()
        .filter_map(|range| {
            let piece = text[range.clone()].trim_end_matches(['\n', '\r']);
            (!piece.trim().is_empty()).then(|| range.start..range.start + piece.len())
        })
        .collect()
}

#[derive(Serialize)]
struct Ask<'a> {
    kind: &'static str,
    state: &'a str,
    question: &'static str,
}

#[derive(Deserialize)]
struct Answer {
    probabilities: Vec<f64>,
}

pub async fn score(endpoint: &Endpoint, chunks: &[Chunk]) -> Result<Vec<f64>, DecideError> {
    let client = reqwest::Client::builder()
        .timeout(CALL_TIMEOUT)
        .build()
        .map_err(DecideError::Client)?;
    let mut unique: HashMap<&str, usize> = HashMap::new();
    let mut texts: Vec<&str> = Vec::new();
    let slots: Vec<usize> = chunks
        .iter()
        .map(|chunk| {
            *unique.entry(&chunk.text).or_insert_with(|| {
                texts.push(&chunk.text);
                texts.len() - 1
            })
        })
        .collect();
    let url = format!("{}/v1/decide", endpoint.url.trim_end_matches('/'));
    let calls: Vec<_> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let (client, url, text) = (client.clone(), url.clone(), (*text).to_owned());
            async move { ask(&client, &url, &text).await.map(|score| (index, score)) }
        })
        .collect();
    let scored: BTreeMap<usize, f64> = futures::stream::iter(calls)
        .buffer_unordered(endpoint.concurrency.get())
        .try_collect()
        .await?;
    slots
        .into_iter()
        .map(|index| {
            scored
                .get(&index)
                .copied()
                .ok_or_else(|| DecideError::Answer {
                    url: url.clone(),
                    detail: format!("no answer for chunk text {index}"),
                })
        })
        .collect()
}

async fn ask(client: &reqwest::Client, url: &str, state: &str) -> Result<f64, DecideError> {
    let unreachable = |source| DecideError::Unreachable {
        url: url.to_owned(),
        source,
    };
    let response = client
        .post(url)
        .json(&Ask {
            kind: "noul",
            state,
            question: QUESTION,
        })
        .send()
        .await
        .map_err(unreachable)?;
    let status = response.status();
    let body = response.text().await.map_err(unreachable)?;
    if !status.is_success() {
        return Err(DecideError::Status {
            url: url.to_owned(),
            status: status.as_u16(),
            body: body.chars().take(200).collect(),
        });
    }
    let answer = |detail: String| DecideError::Answer {
        url: url.to_owned(),
        detail,
    };
    let parsed: Answer = serde_json::from_str(&body).map_err(|error| answer(error.to_string()))?;
    match parsed.probabilities.as_slice() {
        [_, yes] if (0.0..=1.0).contains(yes) => Ok(*yes),
        other => Err(answer(format!("probabilities were {other:?}"))),
    }
}

struct Ranked {
    score: f64,
    text: String,
    chunks: Vec<Chunk>,
}

pub(super) fn flag(
    sessions: &mut [BundledSession],
    candidates: Candidates,
    scores: &[f64],
    review: Review,
) -> Decision {
    let Candidates { chunks, trivial } = candidates;
    let total = chunks.len();
    let calls = chunks
        .iter()
        .map(|chunk| chunk.text.as_str())
        .collect::<HashSet<_>>()
        .len();
    let mut ranked: BTreeMap<usize, Vec<Ranked>> = BTreeMap::new();
    let mut seen: HashMap<(usize, String), usize> = HashMap::new();
    for (chunk, &score) in chunks.into_iter().zip(scores) {
        if score < review.threshold {
            continue;
        }
        let group = ranked.entry(chunk.session).or_default();
        match seen.entry((chunk.session, chunk.text.clone())) {
            Entry::Occupied(at) => {
                if let Some(known) = group.get_mut(*at.get()) {
                    known.chunks.push(chunk);
                }
            }
            Entry::Vacant(at) => {
                at.insert(group.len());
                group.push(Ranked {
                    score,
                    text: chunk.text.clone(),
                    chunks: vec![chunk],
                });
            }
        }
    }
    let mut flags = Vec::new();
    let mut withheld: BTreeMap<usize, Vec<(Range<usize>, usize)>> = BTreeMap::new();
    for (session, mut group) in ranked {
        group.sort_by(|a, b| b.score.total_cmp(&a.score));
        group.truncate(review.top.get());
        for Ranked {
            score,
            text,
            chunks,
        } in group
        {
            let number = flags.len() + 1;
            if review.mode == Mode::Block {
                for chunk in &chunks {
                    withheld
                        .entry(chunk.slot)
                        .or_default()
                        .push((chunk.range.clone(), number));
                }
            }
            flags.push(Flag {
                number,
                session: sessions
                    .get(session)
                    .map(|bundled| bundled.origin.session_key.clone())
                    .unwrap_or_default(),
                places: chunks.into_iter().map(|chunk| chunk.id).collect(),
                score,
                original: text,
            });
        }
    }
    let mut slot = 0;
    visit_texts(sessions, &mut |_, _, _, text| {
        if let Some(ranges) = withheld.get_mut(&slot) {
            ranges.sort_by_key(|(range, _)| range.start);
            for (range, number) in ranges.iter().rev() {
                text.replace_range(range.clone(), &placeholder(*number));
            }
        }
        slot += 1;
    });
    Decision {
        scored: total,
        calls,
        trivial,
        review,
        flags,
    }
}

pub(super) struct Marked<'a> {
    pub number: usize,
    pub resolution: &'a Resolution,
    pub original: &'a str,
    pub places: &'a [String],
}

pub(super) fn resolve(sessions: &mut [BundledSession], marks: &[Marked<'_>]) {
    let mut by_place: HashMap<&str, Vec<&Marked<'_>>> = HashMap::new();
    for mark in marks {
        for place in mark.places {
            let field = place
                .rsplit_once(':')
                .map_or(place.as_str(), |(field, _)| field);
            by_place.entry(field).or_default().push(mark);
        }
    }
    visit_texts(sessions, &mut |_, owner, field, text| {
        let Some(marks) = by_place.get(format!("{owner}:{field}").as_str()) else {
            return;
        };
        for mark in marks {
            let applied = mark.resolution.apply(mark.original);
            let withheld = placeholder(mark.number);
            if text.contains(&withheld) {
                *text = text.replace(&withheld, &applied);
            } else if !applied.contains(mark.original) && text.contains(mark.original) {
                *text = text.replace(mark.original, &applied);
            }
        }
    });
}

#[must_use]
pub fn withheld(sessions: &[BundledSession]) -> usize {
    let mut sessions = sessions.to_vec();
    let mut open = HashSet::new();
    visit_texts(&mut sessions, &mut |_, _, _, text| {
        open.extend(placeholders(text).into_iter().map(|(_, number)| number));
    });
    open.len()
}

fn placeholder(number: usize) -> String {
    format!("{FLAG_OPEN}{number}]")
}

fn placeholders(text: &str) -> Vec<(Range<usize>, usize)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = text[from..].find(FLAG_OPEN) {
        let start = from + offset;
        let digits = start + FLAG_OPEN.len();
        let length = text[digits..]
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len() - digits);
        from = digits;
        if length == 0 || !text[digits + length..].starts_with(']') {
            continue;
        }
        if let Ok(number) = text[digits..digits + length].parse() {
            found.push((start..digits + length + 1, number));
        }
    }
    found
}

fn visit_texts(
    sessions: &mut [BundledSession],
    visit: &mut impl FnMut(usize, &str, &Field, &mut String),
) {
    for (index, session) in sessions.iter_mut().enumerate() {
        let owner = session.origin.session_key.as_str();
        let batch = &mut session.batch;
        if let Some(title) = &mut batch.title {
            visit(index, owner, &Field::Title, title);
        }
        if let Some(settings) = &mut batch.settings {
            visit_object(
                owner,
                Field::Settings,
                settings,
                &mut |owner, field, text| {
                    visit(index, owner, field, text);
                },
            );
        }
        for message in &mut batch.messages {
            let owner = message.event_id.as_str();
            visit(index, owner, &Field::Content, &mut message.content);
            if let Some(result) = &mut message.tool_result {
                visit(index, owner, &Field::ToolResult, result);
            }
            let mut inner = |owner: &str, field: &Field, text: &mut String| {
                visit(index, owner, field, text);
            };
            if let Some(arguments) = &mut message.tool_args {
                visit_object(owner, Field::ToolArgs, arguments, &mut inner);
            }
            if let Some(metadata) = &mut message.metadata {
                visit_object(owner, Field::Metadata, metadata, &mut inner);
            }
        }
    }
}

fn visit_object(
    owner: &str,
    field: fn(Spot) -> Field,
    object: &mut wire::JsonObject,
    visit: &mut impl FnMut(&str, &Field, &mut String),
) {
    object.scrub(&mut Values {
        owner,
        field,
        visit,
    });
}

struct Values<'a, F> {
    owner: &'a str,
    field: fn(Spot) -> Field,
    visit: &'a mut F,
}

impl<F: FnMut(&str, &Field, &mut String)> Scrub for Values<'_, F> {
    fn is_secret_name(&self, _key: &str) -> bool {
        false
    }

    fn rank(&self, _key: &str) -> u64 {
        0
    }

    fn key(&mut self, _parent: &str, _key: &mut String) -> usize {
        0
    }

    fn renamed(
        &mut self,
        _parent: &str,
        _markers: (usize, Option<usize>),
        _key: &str,
        _renamed: &str,
    ) {
    }

    fn text(&mut self, path: &str, text: &mut String) {
        let spot = (self.field)(Spot {
            path: path.to_owned(),
            key: false,
        });
        (self.visit)(self.owner, &spot, text);
    }

    fn secret(&mut self, _path: &str, _key: &str, _text: &mut String) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(text: &str) -> Vec<&str> {
        split(text).into_iter().map(|range| &text[range]).collect()
    }

    #[test]
    fn chunks_pack_whole_lines_up_to_the_limit() {
        let line = format!("{}\n", "a".repeat(700));
        let text = format!("{line}{line}{line}");
        assert_eq!(
            pieces(&text),
            vec![&line[..700], &line[..700], &line[..700]]
        );
        assert_eq!(pieces("one\ntwo\n"), vec!["one\ntwo"]);
        assert_eq!(pieces("\n  \n"), Vec::<&str>::new());
    }

    #[test]
    fn a_line_over_the_limit_is_cut_at_char_boundaries() {
        let text = "é".repeat(CHUNK_CHARS * 2 + 5);
        let cut = pieces(&text);
        assert_eq!(
            cut.iter()
                .map(|piece| piece.chars().count())
                .collect::<Vec<_>>(),
            vec![CHUNK_CHARS, CHUNK_CHARS, 5]
        );
        assert_eq!(cut.concat(), text);
    }

    #[test]
    fn single_tokens_are_trivial_and_text_or_json_is_not() {
        for trivial in [
            "true",
            "42",
            "0f3c2a9e-6d1b-4c55-9a0e-2b7d8f1c4e60",
            "/home/<user>/.claude/projects/x.jsonl",
            "PostToolUse",
            "[REDACTED:secret_key]",
            "",
        ] {
            assert!(is_trivial(trivial), "{trivial}");
        }
        for scored in [
            "Initech ledger",
            "grep churn data.csv",
            r#"{"customer":"Initech","arr":4200000}"#,
            "it's",
        ] {
            assert!(!is_trivial(scored), "{scored}");
        }
    }

    #[test]
    fn placeholders_are_found_by_number() {
        let text = "a [FLAGGED:12] b [FLAGGED:x] [FLAGGED:3]";
        let found: Vec<usize> = placeholders(text).into_iter().map(|(_, n)| n).collect();
        assert_eq!(found, vec![12, 3]);
    }
}
