use std::collections::BTreeMap;
use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use kernel::memory::{MemoryEntry, MemorySearchResult};
use kernel::memory::{MemoryHitSource, TagWire};
use kernel::memory::{SearchOutcome, SearchQuery, SummaryDetail};
use wire::capture::SessionOrigin;

use crate::{Error, render_memory, render_tags};

pub(crate) fn age(date: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let delta = now.signed_duration_since(date);
    if delta.num_minutes() < 1 {
        "just now".to_owned()
    } else if delta.num_hours() < 1 {
        format!("{}m ago", delta.num_minutes())
    } else if delta.num_days() < 1 {
        format!("{}h ago", delta.num_hours())
    } else {
        format!("{}d ago", delta.num_days())
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Clock {
    pub(crate) now: DateTime<Utc>,
    pub(crate) zone: chrono_tz::Tz,
}

pub(crate) fn search(
    query: &SearchQuery,
    outcome: &SearchOutcome,
    messages: &BTreeMap<String, MemoryEntry>,
    tags: &BTreeMap<String, Vec<TagWire>>,
    expand: bool,
    clock: Clock,
) -> Result<String, Error> {
    let Clock { now, zone } = clock;
    let mut text = String::new();
    if let Some(degraded) = &outcome.degraded {
        write!(text, "⚠ {}\n{}\n\n", degraded.reason, degraded.effect)?;
    }
    if let Some(synthesis) = &outcome.synthesis {
        text.push_str(synthesis);
        text.push_str("\n\n");
    }
    if outcome.results.is_empty() {
        text.push_str(&empty(query));
        return Ok(text);
    }
    let query = &query.text;
    let summaries: Vec<_> = outcome
        .results
        .iter()
        .filter(|hit| hit.source == MemoryHitSource::Summary)
        .collect();
    let hits: Vec<_> = outcome
        .results
        .iter()
        .filter(|hit| hit.source == MemoryHitSource::Message)
        .collect();
    write!(
        text,
        "## Memory Search: \"{query}\"\nFound {} results ({} summaries, {} messages)",
        outcome.results.len(),
        summaries.len(),
        hits.len()
    )?;
    if !summaries.is_empty() {
        text.push_str(if expand {
            "\n\n### Summaries (expanded)\n\n"
        } else {
            "\n\n### Summaries\n\n"
        });
        let mut blocks = Vec::new();
        for hit in summaries {
            blocks.push(match outcome.summaries.get(&hit.id) {
                Some(detail) => summary(hit, detail, expand, now, zone)?,
                None => format!(
                    "- [summary/{}] ({}, score: {:.3}) {}",
                    hit.id,
                    dated(hit, now, zone),
                    hit.relevance_score,
                    hit.content
                ),
            });
        }
        text.push_str(&blocks.join(if expand { "\n\n" } else { "\n" }));
    }
    if !hits.is_empty() {
        text.push_str("\n\n### Messages\n\n");
        let mut blocks = Vec::new();
        for hit in hits {
            let entry = messages.get(&hit.id).ok_or_else(|| {
                Error::Invalid(format!("message {} disappeared during search", hit.id))
            })?;
            blocks.push(message(hit, entry, tags, now, zone)?);
        }
        text.push_str(&blocks.join("\n"));
    }
    Ok(text)
}

fn message(
    hit: &MemorySearchResult,
    entry: &MemoryEntry,
    tags: &BTreeMap<String, Vec<TagWire>>,
    now: DateTime<Utc>,
    zone: chrono_tz::Tz,
) -> Result<String, Error> {
    let mut heading = format!("- [{}/{}]", entry.agent, entry.role);
    if let Some(name) = &entry.tool_name {
        write!(heading, " [tool: {name}]")?;
    }
    write!(
        heading,
        " ({}, score: {:.3})",
        dated(hit, now, zone),
        hit.relevance_score
    )?;
    if let Some(origin) = origin(entry)? {
        write!(heading, " from {}", origin.author)?;
        if let Some(created) = origin.created_at {
            write!(heading, " · {}", render_memory::local_date(created, zone))?;
        }
    }
    if let Some(tags) = render_memory::tags(entry, tags) {
        heading.push_str(&tags);
    }
    let payload = match &hit.chunk {
        Some(chunk) => format!(
            "…[chunk {}/{} · chars {}-{} → memory_get_full id={}]…\n{}",
            u64::from(chunk.index) + 1,
            chunk.total,
            chunk.char_start,
            chunk.char_end,
            hit.id,
            render_memory::preview(&chunk.content, 1500)
        ),
        None => render_memory::payload(entry, 500)?,
    };
    Ok(format!("{heading}\n{payload}"))
}

#[derive(serde::Deserialize)]
struct Imported {
    origin: Option<SessionOrigin>,
}

fn origin(entry: &MemoryEntry) -> Result<Option<SessionOrigin>, Error> {
    Ok(match &entry.metadata {
        Some(metadata) => metadata.deserialize::<Imported>()?.origin,
        None => None,
    })
}

fn empty(query: &SearchQuery) -> String {
    let mut bounds = Vec::new();
    if query.since.is_none() && query.before.is_none() {
        if let Some(window) = query.window {
            bounds.push(format!("window=\"{window}\""));
        }
    } else {
        if let Some(since) = query.since {
            bounds.push(format!(
                "since=\"{}\"",
                since.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
            ));
        }
        if let Some(before) = query.before {
            bounds.push(format!(
                "before=\"{}\"",
                before.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
            ));
        }
    }
    if bounds.is_empty() {
        format!(
            "No results found for query \"{}\".\n\nIf you expected a hit: retry with `mode=\"trigram\"` for literal tokens (IPs, hostnames, error strings), or vary the angle (service / host / subnet / role) and try two more queries before trusting the empty result. For time-bounded questions (\"today\", \"yesterday\", \"last week\"), prefer `memory_browse` with window= — search ANDs the query with any date filter and returns empty when FTS misses.",
            query.text
        )
    } else {
        let bounds = bounds.join(", ");
        format!(
            "No results found for query \"{}\" with {bounds}.\n\nFor chronological browsing of a date window without a topic filter, call `memory_browse({bounds})` instead — that returns every message in the window, no FTS match required.",
            query.text
        )
    }
}

fn dated(hit: &MemorySearchResult, now: DateTime<Utc>, zone: chrono_tz::Tz) -> String {
    format!(
        "{} · {}",
        age(hit.created_at, now),
        render_memory::timestamp(hit.created_at, zone)
    )
}

fn summary(
    hit: &MemorySearchResult,
    detail: &SummaryDetail,
    expand: bool,
    now: DateTime<Utc>,
    zone: chrono_tz::Tz,
) -> Result<String, Error> {
    let tags = if detail.tags.is_empty() {
        String::new()
    } else {
        format!(" tags: {}", render_tags::labels(&detail.tags))
    };
    if !expand {
        return Ok(format!(
            "- [{}/{}] ({}, score: {:.3}){tags} {}",
            detail.kind,
            hit.id,
            dated(hit, now, zone),
            hit.relevance_score,
            hit.content
        ));
    }
    let mut text = format!(
        "**[{}]** ({}, score: {:.3}",
        detail.kind,
        dated(hit, now, zone),
        hit.relevance_score,
    );
    if let (Some(earliest), Some(latest)) = (detail.earliest_at, detail.latest_at) {
        write!(
            text,
            ", period: {} → {}",
            render_memory::local_date(earliest, zone),
            render_memory::local_date(latest, zone)
        )?;
    }
    write!(text, "){tags}\n{}", hit.content)?;
    if !detail.children.is_empty() {
        write!(text, "\n\n  **Children ({}):**", detail.children.len())?;
        for child in &detail.children {
            write!(
                text,
                "\n  - [{}] {}",
                child.kind,
                render_memory::preview(&child.content, 500)
            )?;
        }
    }
    if !detail.sources.is_empty() {
        write!(
            text,
            "\n\n  **Source messages ({}):**",
            detail.sources.len()
        )?;
        for source in detail.sources.iter().take(4) {
            write!(
                text,
                "\n  > [{}] {}",
                source.role,
                render_memory::preview(&source.content, 500)
            )?;
        }
        if detail.sources.len() > 4 {
            write!(
                text,
                "\n  > ... and {} more messages",
                detail.sources.len() - 4
            )?;
        }
    }
    Ok(text)
}
