use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::provenance::Origins;
use crate::{Error, render_memory, render_tags};
use chrono::{DateTime, Utc};
use kernel::memory::{MemoryEntry, MemorySearchResult};
use kernel::memory::{MemoryHitSource, Provenance, TagWire};
use kernel::memory::{SearchOutcome, SearchQuery, SummaryDetail};

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

pub(crate) struct HitContext {
    pub(crate) messages: BTreeMap<String, MemoryEntry>,
    pub(crate) tags: BTreeMap<String, Vec<TagWire>>,
    pub(crate) origins: Origins,
}

pub(crate) fn search(
    query: &SearchQuery,
    outcome: &SearchOutcome,
    found: &HitContext,
    expand: bool,
    clock: Clock,
) -> Result<String, Error> {
    let Clock { now, zone } = clock;
    let mut text = String::new();
    if let Some(degraded) = &outcome.degraded {
        writeln!(text, "{}\n", banner(degraded))?;
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
            let origin = hit
                .session_id
                .as_deref()
                .and_then(|key| found.origins.get(key));
            let block = if let Some(detail) = outcome.summaries.get(&hit.id) {
                summary(hit, detail, expand, origin, now, zone)?
            } else {
                let lead = format!(
                    "- [summary/{}] ({}, score: {:.3})",
                    hit.id,
                    dated(hit, now, zone),
                    hit.relevance_score,
                );
                attributed(&lead, &hit.content, origin, zone)
            };
            blocks.push(block);
        }
        text.push_str(&blocks.join(if expand { "\n\n" } else { "\n" }));
    }
    if !hits.is_empty() {
        text.push_str("\n\n### Messages\n\n");
        let mut blocks = Vec::new();
        for hit in hits {
            let entry = found.messages.get(&hit.id).ok_or_else(|| {
                Error::Invalid(format!("message {} disappeared during search", hit.id))
            })?;
            blocks.push(message(hit, entry, found, now, zone)?);
        }
        text.push_str(&blocks.join("\n"));
    }
    Ok(text)
}

fn message(
    hit: &MemorySearchResult,
    entry: &MemoryEntry,
    found: &HitContext,
    now: DateTime<Utc>,
    zone: chrono_tz::Tz,
) -> Result<String, Error> {
    let origin = found.origins.get(&entry.session_id);
    let imported = origin.is_some();
    let mut heading = format!(
        "- [{}/{}]",
        crate::provenance::own_text(imported, &entry.agent),
        entry.role
    );
    if let Some(name) = &entry.tool_name {
        write!(
            heading,
            " [tool: {}]",
            crate::provenance::own_text(imported, name)
        )?;
    }
    write!(
        heading,
        " ({}, score: {:.3})",
        dated(hit, now, zone),
        hit.relevance_score
    )?;
    if let Some(provenance) = origin {
        write!(
            heading,
            " {}",
            crate::provenance::tag(provenance, zone, true)
        )?;
    }
    if let Some(tags) = render_memory::tags(entry, &found.tags) {
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
    let payload = match origin {
        Some(provenance) => kernel::memory::quote(provenance, &payload),
        None => payload,
    };
    Ok(format!("{heading}\n{payload}"))
}

fn attributed(
    lead: &str,
    content: &str,
    origin: Option<&Provenance>,
    zone: chrono_tz::Tz,
) -> String {
    match origin {
        Some(provenance) => format!(
            "{lead} {}\n{}",
            crate::provenance::tag(provenance, zone, true),
            kernel::memory::quote(provenance, content)
        ),
        None => format!("{lead} {content}"),
    }
}

const UNCONFIGURED: &str = "embedding endpoint not configured";
const KEYWORD_NOTE: &str = "Keyword search. Meaning-based search needs an embedding endpoint.";

fn banner(degraded: &kernel::memory::MemoryDegraded) -> String {
    if degraded.reason == UNCONFIGURED {
        KEYWORD_NOTE.to_owned()
    } else {
        format!("⚠ {}\n{}", degraded.reason, degraded.effect)
    }
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
    origin: Option<&Provenance>,
    now: DateTime<Utc>,
    zone: chrono_tz::Tz,
) -> Result<String, Error> {
    let tags = if detail.tags.is_empty() {
        String::new()
    } else {
        format!(" tags: {}", render_tags::labels(&detail.tags))
    };
    if !expand {
        let lead = format!(
            "- [{}/{}] ({}, score: {:.3}){tags}",
            detail.kind,
            hit.id,
            dated(hit, now, zone),
            hit.relevance_score,
        );
        return Ok(attributed(&lead, &hit.content, origin, zone));
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
    text.push(')');
    if let Some(provenance) = origin {
        write!(text, " {}", crate::provenance::tag(provenance, zone, true))?;
    }
    text.push_str(&tags);
    let mut body = hit.content.clone();
    if !detail.children.is_empty() {
        write!(body, "\n\n  **Children ({}):**", detail.children.len())?;
        for child in &detail.children {
            write!(
                body,
                "\n  - [{}] {}",
                child.kind,
                render_memory::preview(&child.content, 500)
            )?;
        }
    }
    if !detail.sources.is_empty() {
        write!(
            body,
            "\n\n  **Source messages ({}):**",
            detail.sources.len()
        )?;
        for source in detail.sources.iter().take(4) {
            write!(
                body,
                "\n  > [{}] {}",
                source.role,
                render_memory::preview(&source.content, 500)
            )?;
        }
        if detail.sources.len() > 4 {
            write!(
                body,
                "\n  > ... and {} more messages",
                detail.sources.len() - 4
            )?;
        }
    }
    text.push('\n');
    text.push_str(&match origin {
        Some(provenance) => kernel::memory::quote(provenance, &body),
        None => body,
    });
    Ok(text)
}
