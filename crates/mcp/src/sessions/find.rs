use std::fmt::Write as _;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::Limit;
use kernel::memory::MemoryScope;
use kernel::memory::MemorySearchResult;
use kernel::memory::MemoryStore;
use kernel::memory::SearchMode;
use kernel::memory::SearchQuery;
use kernel::memory::SessionDigest;
use rmcp::model::CallToolResult;

use super::Find;
use super::Sessions;
use super::noun;
use crate::Error;
use crate::catalog::ToolInput as _;

const HITS_PER_SESSION: u32 = 4;
const MAX_HITS: u32 = 100;
const EXCERPTS: usize = 3;
const EXCERPT_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ranked {
    pub(crate) key: String,
    pub(crate) best: f64,
    pub(crate) hits: usize,
    pub(crate) latest: DateTime<Utc>,
    pub(crate) excerpts: Vec<String>,
}

pub(crate) fn group(results: &[MemorySearchResult]) -> Vec<Ranked> {
    let mut ranked: Vec<Ranked> = Vec::new();
    for hit in results {
        let Some(key) = &hit.session_id else {
            continue;
        };
        let excerpt = excerpt(&hit.content);
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

pub(crate) async fn find(
    store: &dyn MemoryStore,
    kit: &Sessions,
    args: Find,
    zone: chrono_tz::Tz,
) -> Result<CallToolResult, Error> {
    let query = args.query.trim().to_owned();
    if query.is_empty() {
        return Err(Error::Invalid(
            "query must name what the sessions were about".into(),
        ));
    }
    let wanted = crate::memory::limit(Find::NAME, args.limit, 10, 25)?.get();
    let outcome = store
        .search(SearchQuery {
            text: query.clone(),
            mode: SearchMode::Hybrid,
            scope: MemoryScope::Both,
            agent: args.agent,
            role: None,
            exclude_roles: Vec::new(),
            tool_name: None,
            tag: args.tag.map(|tag| tag.parse()).transpose().map_err(
                |error: kernel::memory::TagLiteralError| Error::Invalid(error.to_string()),
            )?,
            limit: Limit::clamped(
                Some(wanted.saturating_mul(HITS_PER_SESSION)),
                MAX_HITS,
                1..=MAX_HITS,
            ),
            since: args.since,
            before: args.before,
            window: None,
            expand: Some(false),
            synthesize: Some(false),
        })
        .await?;
    let ranked = group(&outcome.results);
    let keys: Vec<String> = ranked.iter().map(|session| session.key.clone()).collect();
    let digests = if keys.is_empty() {
        Vec::new()
    } else {
        kit.archive.digests(&keys).await?
    };
    let mut found: Vec<(&Ranked, &SessionDigest)> = Vec::new();
    let mut dropped = 0;
    for session in &ranked {
        match digests.iter().find(|digest| digest.key == session.key) {
            Some(digest) => found.push((session, digest)),
            None => dropped += session.hits,
        }
    }
    found.truncate(wanted as usize);
    Ok(crate::text(render(&query, &found, dropped, zone)?))
}

fn render(
    query: &str,
    found: &[(&Ranked, &SessionDigest)],
    dropped: usize,
    zone: chrono_tz::Tz,
) -> Result<String, Error> {
    let mut out = String::new();
    if found.is_empty() {
        writeln!(out, "No sessions match \"{query}\".")?;
    }
    let day = |at: DateTime<Utc>| at.with_timezone(&zone).format("%Y-%m-%d").to_string();
    for (index, (ranked, digest)) in found.iter().enumerate() {
        write!(
            out,
            "{}. {}  \"{}\"  {}  {} to {}  {}  best {:.2} ({})",
            index + 1,
            digest.conversation_id,
            digest.title.as_deref().unwrap_or("untitled"),
            digest.agent,
            day(digest.first),
            day(digest.last),
            noun(digest.messages, "message", "messages"),
            ranked.best,
            noun(ranked.hits, "hit", "hits"),
        )?;
        if let Some(author) = &digest.imported_from {
            write!(out, "  from {author}")?;
        }
        out.push('\n');
        for excerpt in &ranked.excerpts {
            writeln!(out, "   > {excerpt}")?;
        }
    }
    if dropped > 0 {
        writeln!(
            out,
            "left out {} from sessions this node cannot export (trashed or not yours)",
            noun(dropped, "hit", "hits")
        )?;
    }
    if !found.is_empty() {
        writeln!(
            out,
            "pass the conversation ids to session_export to write them to a bundle"
        )?;
    }
    Ok(out)
}
