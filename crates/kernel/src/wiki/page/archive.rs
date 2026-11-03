use std::collections::BTreeSet;

use chrono::{DateTime, NaiveDate, Utc};

use super::bound::{ARCHIVED_TITLE, Archived, blocks, covered, history_line, spill, words};
use super::limits;
use super::{WikiHistoryEntry, WikiPage};

pub const ARCHIVE_DIRECTORY: &str = "archive";
const MONTH: &str = "%Y-%m";
pub const SEGMENT_BUDGET: usize = 64 * 1024;

#[must_use]
pub fn archive_month(at: DateTime<Utc>) -> String {
    at.format(MONTH).to_string()
}

#[must_use]
pub fn parse_archive_month(text: &str) -> Option<String> {
    let first = format!("{text}-01");
    let valid = text.len() == 7 && NaiveDate::parse_from_str(&first, "%Y-%m-%d").is_ok();
    valid.then(|| text.to_owned())
}

#[must_use]
pub fn archive_directory(slug: &str) -> String {
    format!("{ARCHIVE_DIRECTORY}/{slug}")
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ArchivePart {
    pub month: String,
    pub part: u32,
}

impl ArchivePart {
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let stem = name.strip_suffix(".md")?;
        let (month, part) = match stem.split_once('.') {
            Some((month, part)) => {
                let number = part.bytes().try_fold(0u32, |number, byte| {
                    let digit = byte.checked_sub(b'0').filter(|digit| *digit < 10)?;
                    number.checked_mul(10)?.checked_add(u32::from(digit))
                })?;
                (number > 0 && number.to_string() == part).then_some((month, number))?
            }
            None => (stem, 0),
        };
        Some(Self {
            month: parse_archive_month(month)?,
            part,
        })
    }

    #[must_use]
    pub fn name(&self) -> String {
        if self.part == 0 {
            format!("{}.md", self.month)
        } else {
            format!("{}.{}.md", self.month, self.part)
        }
    }
}

#[must_use]
pub fn archive_path(slug: &str, part: &ArchivePart) -> String {
    format!("{ARCHIVE_DIRECTORY}/{slug}/{}", part.name())
}

#[must_use]
pub fn next_part(month: &str, parts: &[(ArchivePart, usize)], adding: usize) -> ArchivePart {
    let newest = parts
        .iter()
        .filter(|(part, _)| part.month == month)
        .max_by_key(|(part, _)| part.part);
    match newest {
        Some((part, size)) if part.part > 0 && size.saturating_add(adding) <= SEGMENT_BUDGET => {
            part.clone()
        }
        Some((part, _)) => ArchivePart {
            month: month.to_owned(),
            part: part.part.saturating_add(1),
        },
        None => ArchivePart {
            month: month.to_owned(),
            part: 1,
        },
    }
}

#[must_use]
pub fn legacy_archive_path(slug: &str) -> String {
    format!("{ARCHIVE_DIRECTORY}/{slug}.md")
}

#[must_use]
pub fn archive_anchor(at: DateTime<Utc>) -> String {
    format!("a-{}", at.format("%Y%m%dT%H%M%S%3fZ"))
}

#[must_use]
pub fn archive_header(title: &str, slug: &str, month: &str) -> String {
    format!(
        "# Archive: {title}, {month}\n\nText moved out of [[{slug}]] in {month} when a rewrite removed it or it went over its size budget, oldest first.\n"
    )
}

#[must_use]
pub fn unarchived(archived: Vec<Archived>, page: &WikiPage, archive: &str) -> Vec<Archived> {
    let mut inline = vec![page.current_state.as_str(), page.article.as_str()];
    inline.extend(
        page.extra_sections
            .iter()
            .map(|section| section.body.as_str()),
    );
    let inline = words(&inline.join("\n\n"));
    let mut stored: Vec<BTreeSet<String>> =
        blocks(archive).iter().map(|block| words(block)).collect();
    let mut kept = Vec::new();
    for item in archived {
        let mut fresh = Vec::new();
        for block in blocks(&item.text) {
            let found = words(&block);
            if covered(&found, &inline) || stored.iter().any(|known| covered(&found, known)) {
                continue;
            }
            stored.push(found);
            fresh.push(block);
        }
        if !fresh.is_empty() {
            kept.push(Archived {
                text: fresh.join("\n\n"),
                ..item
            });
        }
    }
    kept
}

#[must_use]
pub fn archive_block(at: DateTime<Utc>, inline: Option<&str>, archived: &[Archived]) -> String {
    let mut block = format!(
        "\n<a id=\"{}\"></a>\n\n## {}",
        archive_anchor(at),
        at.format("%Y-%m-%d %H:%M UTC")
    );
    if let Some(sha) = inline {
        block.push_str(" (last inline at ");
        block.push_str(short(sha));
        block.push(')');
    }
    block.push('\n');
    for item in archived {
        block.push_str("\n### ");
        block.push_str(&item.label);
        block.push_str("\n\n");
        block.push_str(&item.text);
        block.push('\n');
    }
    block
}

#[must_use]
pub fn archive_entry(
    slug: &str,
    at: DateTime<Utc>,
    inline: Option<&str>,
    archived: &[Archived],
) -> WikiHistoryEntry {
    let bytes: usize = archived.iter().map(|item| item.text.len()).sum();
    let mut labels: Vec<&str> = Vec::new();
    for item in archived {
        if !labels.contains(&item.label.as_str()) {
            labels.push(&item.label);
        }
    }
    let named = if labels.len() > 3 {
        format!(
            "{} and {} more",
            labels.get(..3).unwrap_or_default().join(", "),
            labels.len().saturating_sub(3)
        )
    } else {
        labels.join(", ")
    };
    let mut body = format!(
        "Moved {} to [the archive](/wiki/{slug}/archive/{}#{}): {named}.",
        size(bytes),
        archive_month(at),
        archive_anchor(at)
    );
    if let Some(sha) = inline {
        body.push_str(" Last inline at `");
        body.push_str(short(sha));
        body.push_str("`.");
    }
    let line = history_line(&body);
    WikiHistoryEntry {
        date: at.date_naive(),
        title: ARCHIVED_TITLE.to_owned(),
        body: line.kept,
    }
}

pub fn settle_archive(
    page: &mut WikiPage,
    at: DateTime<Utc>,
    inline: Option<&str>,
    archived: &mut Vec<Archived>,
) -> Option<String> {
    if archived.is_empty() {
        return None;
    }
    let today = at.date_naive();
    let noted = page
        .history
        .iter()
        .any(|entry| entry.title == ARCHIVED_TITLE && entry.date == today);
    if archived.iter().any(|item| item.notice) && !noted {
        while page.history.len() >= limits::HISTORY {
            let Some(entry) = page.history.pop() else {
                break;
            };
            spill(archived, &entry);
        }
        let slug = page.meta.slug.as_ref().to_owned();
        page.history
            .insert(0, archive_entry(&slug, at, inline, archived));
    }
    Some(archive_block(at, inline, archived))
}

fn short(sha: &str) -> &str {
    sha.get(..10).unwrap_or(sha)
}

fn size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        let tenths = bytes.saturating_mul(10) / 1024;
        format!("{}.{} KB", tenths / 10, tenths % 10)
    }
}
