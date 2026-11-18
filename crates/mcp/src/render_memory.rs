use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use wire::time::LegacyStrftime as _;
use wire::time::TimestampFormat as _;

use kernel::memory::BrowseFilter;
use kernel::memory::MemoryEntry;
use kernel::memory::{MemoryBrowseOrder, TagWire};
use serde::Deserialize;

use crate::Error;
use crate::provenance::Origins;

#[derive(Debug, Deserialize)]
pub(crate) struct CaptureMetadata {
    pub(crate) truncated: Option<bool>,
    pub(crate) full_content_length: Option<usize>,
    pub(crate) full_tool_result_length: Option<usize>,
    pub(crate) session_jsonl_path: Option<String>,
}

pub(crate) fn metadata(entry: &MemoryEntry) -> Result<Option<CaptureMetadata>, Error> {
    entry
        .metadata
        .as_ref()
        .map(|value| Ok(serde_json::from_value(serde_json::to_value(value)?)?))
        .transpose()
}

pub(crate) fn preview(value: &str, cap: usize) -> String {
    let mut chars = value.chars();
    let mut text: String = chars.by_ref().take(cap).collect();
    if chars.next().is_some() {
        text.push('…');
    }
    text
}

pub(crate) fn payload(entry: &MemoryEntry, cap: usize) -> Result<String, Error> {
    let mut text = preview(&entry.content, cap);
    let metadata = metadata(entry)?;
    let truncated = metadata
        .as_ref()
        .is_some_and(|metadata| metadata.truncated == Some(true));
    if let Some(result) = &entry.tool_result {
        text.push_str("\n[tool_result");
        if let Some(name) = &entry.tool_name {
            write!(text, " ({name})")?;
        }
        let length = result.chars().count();
        if length > cap {
            write!(text, " {length} chars")?;
        }
        text.push_str("]\n");
        text.push_str(&preview(result, cap));
        if length > cap
            && !truncated
            && let Some(id) = &entry.id
        {
            write!(
                text,
                "\n…[display-truncated tool_result → memory_get_full id={id}]"
            )?;
        }
    }
    if let Some(metadata) = metadata.filter(|metadata| metadata.truncated == Some(true)) {
        text.push_str("\n⚠ truncated at capture");
        if let Some(length) = metadata
            .full_tool_result_length
            .or(metadata.full_content_length)
        {
            write!(text, " (full: {length} chars)")?;
        }
        if let Some(id) = &entry.id {
            write!(text, " → memory_get_full id={id}")?;
        }
    }
    Ok(text)
}

pub(crate) fn tags(
    entry: &MemoryEntry,
    sessions: &BTreeMap<String, Vec<TagWire>>,
) -> Option<String> {
    sessions
        .get(&entry.session_id)
        .filter(|tags| !tags.is_empty())
        .map(|tags| format!(" tags: {}", crate::render_tags::labels(tags)))
}

pub(crate) fn browse(
    entries: &[MemoryEntry],
    filter: &BrowseFilter,
    sessions: &BTreeMap<String, Vec<TagWire>>,
    origins: &Origins,
    zone: &TimeZone,
) -> Result<String, Error> {
    let mut details = Vec::new();
    if let Some(window) = filter.window {
        details.push(format!("window=\"{window}\""));
    }
    if let Some(since) = filter.since {
        let since = if filter.window.is_some() {
            since.rfc3339_millis()
        } else {
            since.rfc3339_z()
        };
        details.push(format!("since={since}"));
    }
    if let Some(before) = filter.before {
        details.push(format!("before={}", before.rfc3339_z()));
    }
    if !filter.include_tools {
        details.push("tools excluded (include_tools=true to show)".to_owned());
    }
    let mut text = if entries.is_empty() {
        "No messages found.".to_owned()
    } else {
        let order = match filter.order {
            MemoryBrowseOrder::Asc => "oldest",
            MemoryBrowseOrder::Desc => "newest",
        };
        format!("## Messages ({} returned, {order} first)", entries.len())
    };
    if !details.is_empty() {
        write!(text, "\n_{}_", details.join(" "))?;
    }
    if entries.is_empty() {
        text.push_str("\n_Try a wider window, drop agent/conversation filters, flip order, pass include_tools=true, or use memory_search for topic recall._");
        return Ok(text);
    }
    if entries.len() as u64 >= u64::from(filter.limit.get()) {
        write!(
            text,
            "\n_Hit limit={}. Flip order, raise limit (max 200), or narrow since/before/window._",
            filter.limit.get()
        )?;
    }
    let mut blocks = Vec::new();
    for entry in entries {
        let mut heading = String::new();
        if let Some(date) = entry.created_at {
            write!(heading, "[{}] ", timestamp(date, zone))?;
        }
        let imported = origins.contains_key(&entry.session_id);
        write!(
            heading,
            "{}/{}",
            crate::provenance::own_text(imported, &entry.agent),
            entry.role
        )?;
        if let Some(name) = &entry.tool_name {
            write!(
                heading,
                " [tool: {}]",
                crate::provenance::own_text(imported, name)
            )?;
        }
        if let Some(provenance) = origins.get(&entry.session_id) {
            write!(
                heading,
                " · {}",
                crate::provenance::tag(provenance, zone, false)
            )?;
        }
        if let Some(tags) = tags(entry, sessions) {
            heading.push_str(&tags);
        }
        let body = crate::provenance::quoted(origins, &entry.session_id, payload(entry, 800)?);
        blocks.push(format!("{heading}\n{body}"));
    }
    text.push_str("\n\n");
    text.push_str(&blocks.join("\n\n---\n\n"));
    Ok(text)
}

pub(crate) fn timestamp(date: Timestamp, zone: &TimeZone) -> String {
    local_time(date, zone, "%Y-%m-%d %H:%M:%S")
}

pub(crate) fn timestamp_minutes(date: Timestamp, zone: &TimeZone) -> String {
    local_time(date, zone, "%Y-%m-%d %H:%M")
}

fn local_time(date: Timestamp, zone: &TimeZone, pattern: &str) -> String {
    let info = display_offset(date, zone);
    let local = date.to_zoned_datetime(TimeZone::fixed(info.offset()));
    format!("{} {}", local.legacy_strftime(pattern), info.abbreviation())
}

fn display_offset(date: Timestamp, zone: &TimeZone) -> jiff::tz::TimeZoneOffsetInfo<'_> {
    const LAST_BUNDLED_RULE_INSTANT: Timestamp = Timestamp::constant(4_102_444_799, 0);
    zone.to_offset_info(date.min(LAST_BUNDLED_RULE_INSTANT))
}

pub(crate) fn local_date(date: Timestamp, zone: &TimeZone) -> String {
    display_offset(date, zone)
        .offset()
        .to_datetime(date)
        .date()
        .legacy_strftime("%Y-%m-%d")
}

#[cfg(test)]
#[test]
fn timestamps_after_the_bundled_rule_horizon_keep_the_final_offset() -> Result<(), jiff::Error> {
    let zone = wire::time::bundled_zone("America/New_York")?;
    let before = "2099-07-01T04:30:00Z".parse()?;
    let after = "2100-07-01T04:30:00Z".parse()?;
    assert_eq!(timestamp(before, &zone), "2099-07-01 00:30:00 EDT");
    assert_eq!(timestamp(after, &zone), "2100-06-30 23:30:00 EST");
    assert_eq!(timestamp_minutes(after, &zone), "2100-06-30 23:30 EST");
    assert_eq!(local_date(after, &zone), "2100-06-30");
    Ok(())
}

pub(crate) fn get_full(
    id: &str,
    entry: Option<&MemoryEntry>,
    origins: &Origins,
) -> Result<String, Error> {
    let Some(entry) = entry else {
        return Ok(format!("No message with id {id}."));
    };
    let text = full_text(entry)?;
    Ok(crate::provenance::quoted(origins, &entry.session_id, text))
}

fn full_text(entry: &MemoryEntry) -> Result<String, Error> {
    let metadata = metadata(entry)?;
    if let Some(metadata) = metadata.filter(|metadata| metadata.truncated == Some(true)) {
        let mut text =
            "Capture payload is truncated; source JSONL recovery is not configured".to_owned();
        if let Some(path) = metadata.session_jsonl_path {
            write!(text, " ({path})")?;
        }
        write!(
            text,
            ". agent={}\n\nRe-run memory_get_full on the capture host to recover the original payload.",
            entry.agent
        )?;
        return Ok(text);
    }
    let mut text = format!(
        "(row was not truncated — stored payload is complete)\n\n{}",
        entry.content
    );
    if let Some(result) = &entry.tool_result {
        text.push_str("\n\n[tool");
        if let Some(name) = &entry.tool_name {
            write!(text, ": {name}")?;
        }
        text.push_str("]\n");
        text.push_str(result);
    }
    Ok(text)
}

pub(crate) fn stats(stats: &kernel::memory::MemoryStatsResponse) -> Result<String, Error> {
    let mut text = format!(
        "## Memory System Health\n\n**Messages:** {}\n\n**Conversations:** {}\n\n**Summaries:** {}\n\n**Embedding queue:** {}\n**Embedded messages:** {}\n**Failed embeddings:** {}",
        stats.messages,
        stats.conversations,
        stats.summaries,
        stats.embed_queue_depth,
        stats.embedded_messages,
        stats.failed_embeddings
    );
    if !stats.top_tools.is_empty() {
        text.push_str("\n\n**Top tools:**");
        for tool in &stats.top_tools {
            write!(text, "\n  {}: {}", tool.tool, tool.count)?;
        }
    }
    Ok(text)
}
