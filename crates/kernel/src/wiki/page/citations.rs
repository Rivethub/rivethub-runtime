use jiff::civil::Date;
use strum::IntoEnumIterator as _;

use super::slug::{is_link_target, normalize_slug};
use super::{CitationKind, PageCitation};

#[must_use]
pub fn parse_see_also(text: &str) -> Vec<String> {
    let mut slugs = Vec::new();
    for line in text.lines() {
        let Some(item) = bullet(line) else {
            continue;
        };
        let target = match item.strip_prefix("[[") {
            Some(link) => link.split_once("]]").map(|(target, _)| target),
            None => Some(item).filter(|item| !item.contains(char::is_whitespace)),
        };
        if let Some(target) = target.filter(|target| is_link_target(target)) {
            let slug = normalize_slug(target);
            if !slug.is_empty() && !slugs.contains(&slug) {
                slugs.push(slug);
            }
        }
    }
    slugs
}

fn bullet(line: &str) -> Option<&str> {
    let line = line.trim();
    let rest = line.strip_prefix(['-', '*'])?;
    rest.starts_with(char::is_whitespace).then(|| rest.trim())
}

#[must_use]
pub fn parse_citations(text: &str) -> Vec<PageCitation> {
    text.lines()
        .filter_map(|line| table_row(line).or_else(|| bullet_entry(line)))
        .collect()
}

fn table_row(line: &str) -> Option<PageCitation> {
    let inner = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    let cells = cells(inner);
    let [date, kind, summary, note] = cells.as_slice() else {
        return None;
    };
    Some(PageCitation {
        summary_id: uuid_text(unquote(summary))?,
        date: iso_date(date),
        kind: citation_kind(kind),
        note: Some(note.clone()).filter(|note| !note.is_empty()),
    })
}

fn cells(row: &str) -> Vec<String> {
    let mut cells = vec![String::new()];
    let mut chars = row.chars().peekable();
    while let Some(ch) = chars.next() {
        match (ch, chars.peek()) {
            ('\\', Some('|')) => {
                chars.next();
                push_char(&mut cells, '|');
            }
            ('|', _) => cells.push(String::new()),
            (ch, _) => push_char(&mut cells, ch),
        }
    }
    cells
        .into_iter()
        .map(|cell| cell.trim().to_owned())
        .collect()
}

fn push_char(cells: &mut [String], ch: char) {
    if let Some(cell) = cells.last_mut() {
        cell.push(ch);
    }
}

fn unquote(text: &str) -> &str {
    let text = text.strip_prefix('`').unwrap_or(text);
    text.strip_suffix('`').unwrap_or(text)
}

fn bullet_entry(line: &str) -> Option<PageCitation> {
    let item = bullet(line)?;
    let unquoted = item.strip_prefix('`').unwrap_or(item);
    let (id, rest) = unquoted.split_at_checked(36)?;
    let summary_id = uuid_text(id)?;
    let rest = rest.strip_prefix('`').unwrap_or(rest);
    let mut words = rest.split_whitespace();
    if let (Some(date), Some(kind), Some("—")) = (words.next(), words.next(), words.next())
        && let (Some(date), Some(kind)) = (iso_date(date), citation_kind(kind))
    {
        let note = rest.split_once('—').map(|(_, note)| note.trim().to_owned());
        return Some(PageCitation {
            summary_id,
            date: Some(date),
            kind: Some(kind),
            note: note.filter(|note| !note.is_empty()),
        });
    }
    Some(PageCitation {
        summary_id,
        date: rest.split_whitespace().find_map(iso_date),
        kind: rest.split_whitespace().find_map(citation_kind),
        note: Some(item.to_owned()),
    })
}

pub(super) fn iso_date(text: &str) -> Option<Date> {
    let mut parts = text.split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    match Date::strptime("%Y-%m-%d", text) {
        Ok(date) => Some(date),
        Err(error) => {
            tracing::trace!(%error, "wiki citation date is invalid");
            None
        }
    }
}

fn citation_kind(text: &str) -> Option<CitationKind> {
    CitationKind::iter().find(|kind| kind.to_string().eq_ignore_ascii_case(text))
}

fn uuid_text(text: &str) -> Option<String> {
    let valid = text.len() == 36
        && text.char_indices().all(|(index, ch)| match index {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        });
    valid.then(|| text.to_ascii_lowercase())
}
