use crate::Error;
use std::collections::BTreeMap;
use std::fmt::Write as _;

use kernel::memory::{
    PendingTagWire, TagCount, TagEntityType, TagState, TagTaxonomyEntry, TagWire,
};

pub(crate) fn label(tag: &TagWire) -> String {
    format!("{}:{}", tag.key, tag.display)
}

pub(crate) fn labels(tags: &[TagWire]) -> String {
    tags.iter().map(label).collect::<Vec<_>>().join(", ")
}

pub(crate) fn list(tags: &[TagWire]) -> String {
    tags.iter()
        .map(|tag| {
            format!(
                "{}  {}  [{}/{}] {}:{}",
                tag.id,
                label(tag),
                tag.state,
                tag.source,
                tag.entity_type,
                tag.entity_id
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn pending(tags: &[PendingTagWire]) -> Result<String, Error> {
    let mut lines = vec![format!("{} pending:", tags.len())];
    for entry in tags {
        let tag = &entry.tag;
        let mut line = format!("{}  {}", tag.id, label(tag));
        if let Some(confidence) = tag.confidence {
            write!(line, " ({confidence:.2})")?;
        }
        match tag.entity_type {
            TagEntityType::Conversation => {
                line.push_str("  ← session ");
                if let Some(key) = &entry.session_key {
                    line.push_str(key);
                }
                if let Some(title) = &entry.title {
                    write!(line, " \"{title}\"")?;
                }
            }
            TagEntityType::Summary => {
                write!(
                    line,
                    "  ← summary {}",
                    tag.entity_id.chars().take(8).collect::<String>()
                )?;
                if let Some(key) = &entry.session_key {
                    write!(line, " of {key}")?;
                }
            }
        }
        if !tag.reason.is_empty() {
            write!(line, "\n    {}", tag.reason)?;
        }
        lines.push(line);
    }
    Ok(lines.join("\n"))
}

pub(crate) fn counts(tags: &[TagCount]) -> String {
    tags.iter()
        .map(|tag| format!("{}:{}  {}", tag.key, tag.display, tag.conversations))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn taxonomy(entries: &[TagTaxonomyEntry]) -> String {
    entries
        .iter()
        .map(|tag| format!("{}:{}  [{}]", tag.key, tag.display, tag.state))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn lookup(keys: &[String], sessions: &BTreeMap<String, Vec<TagWire>>) -> String {
    keys.iter()
        .map(|key| {
            let labels = match sessions.get(key).filter(|tags| !tags.is_empty()) {
                Some(tags) => tags
                    .iter()
                    .map(|tag| {
                        let mut text = label(tag);
                        if tag.state == TagState::Suggested {
                            text.push('?');
                        }
                        text
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                None => "(none)".to_owned(),
            };
            format!("{key}: {labels}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
