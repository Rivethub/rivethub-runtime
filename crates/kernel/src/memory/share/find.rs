use chrono::DateTime;
use chrono::Utc;

use crate::memory::Limit;
use crate::memory::MemoryScope;
use crate::memory::MemorySearchResult;
use crate::memory::SearchMode;
use crate::memory::SearchQuery;
use crate::memory::SessionDigest;
use crate::memory::TagLiteral;

pub const DEFAULT_SESSIONS: u32 = 10;
pub const MAX_SESSIONS: u32 = 25;
const HITS_PER_SESSION: u32 = 4;
const MAX_HITS: u32 = 100;
const EXCERPTS: usize = 3;
const EXCERPT_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub text: String,
    pub agent: Option<String>,
    pub tag: Option<TagLiteral>,
    pub since: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
}

#[must_use]
pub fn query(search: Search, wanted: u32) -> SearchQuery {
    SearchQuery {
        text: search.text,
        mode: SearchMode::Hybrid,
        scope: MemoryScope::Both,
        agent: search.agent,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: search.tag,
        limit: Limit::clamped(
            Some(wanted.saturating_mul(HITS_PER_SESSION)),
            MAX_HITS,
            1..=MAX_HITS,
        ),
        since: search.since,
        before: search.before,
        window: None,
        expand: Some(false),
        synthesize: Some(false),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub key: String,
    pub best: f64,
    pub hits: usize,
    pub latest: DateTime<Utc>,
    pub excerpts: Vec<String>,
}

pub fn group(results: &[MemorySearchResult], redact: impl Fn(&str) -> String) -> Vec<Ranked> {
    let mut ranked: Vec<Ranked> = Vec::new();
    for hit in results {
        let Some(key) = &hit.session_id else {
            continue;
        };
        let excerpt = excerpt(&redact(&hit.content));
        match ranked.iter_mut().find(|session| session.key == *key) {
            Some(session) => {
                session.best = session.best.max(hit.relevance_score);
                session.hits += 1;
                session.latest = session.latest.max(hit.created_at);
                if session.excerpts.len() < EXCERPTS && !session.excerpts.contains(&excerpt) {
                    session.excerpts.push(excerpt);
                }
            }
            None => ranked.push(Ranked {
                key: key.clone(),
                best: hit.relevance_score,
                hits: 1,
                latest: hit.created_at,
                excerpts: vec![excerpt],
            }),
        }
    }
    ranked.sort_by(|left, right| {
        right
            .best
            .total_cmp(&left.best)
            .then_with(|| right.hits.cmp(&left.hits))
            .then_with(|| right.latest.cmp(&left.latest))
    });
    ranked
}

fn excerpt(content: &str) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= EXCERPT_CHARS {
        return flat;
    }
    let mut cut: String = flat.chars().take(EXCERPT_CHARS - 1).collect();
    cut.push('…');
    cut
}

#[derive(Debug, Clone, PartialEq)]
pub struct Matched<'a> {
    pub found: Vec<(&'a Ranked, &'a SessionDigest)>,
    pub dropped: usize,
}

#[must_use]
pub fn matched<'a>(ranked: &'a [Ranked], digests: &'a [SessionDigest], wanted: u32) -> Matched<'a> {
    let mut found = Vec::new();
    let mut dropped = 0;
    for session in ranked {
        match digests.iter().find(|digest| digest.key == session.key) {
            Some(digest) => found.push((session, digest)),
            None => dropped += session.hits,
        }
    }
    found.truncate(wanted as usize);
    Matched { found, dropped }
}
