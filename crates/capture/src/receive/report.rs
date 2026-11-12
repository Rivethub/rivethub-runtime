use std::fmt;
use std::ops::Range;

use wire::capture::SessionBundle;

use super::Checked;
use super::Family;
use super::Finding;

const EXCERPT_CHARS: usize = 120;
const MARKERS: usize = 4;
const SPAN_CHARS: usize = 60;
const SPAN_CHARS_WITH_NOTE: usize = 30;
const SPELLED_CHARS: usize = 40;
const CONTEXT_BYTES: usize = 80;
const SHOWN_CHARS: usize = 4096;
const TITLE_CHARS: usize = 60;
const ROWS_PER_SESSION: usize = 20;

#[must_use]
pub fn visible(text: &str) -> String {
    text.chars().map(token).collect()
}

fn token(c: char) -> String {
    match c {
        '\n' => "\\n".to_owned(),
        '\r' => "\\r".to_owned(),
        '\t' => "\\t".to_owned(),
        c if hidden(c) => format!("<U+{:04X}>", u32::from(c)),
        c => c.to_string(),
    }
}

fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            u32::from(c),
            0xAD | 0x61C
                | 0x180E
                | 0x200B..=0x200F
                | 0x2028..=0x202E
                | 0x2060..=0x206F
                | 0xFE00..=0xFE0F
                | 0xFEFF
                | 0xFFF9..=0xFFFC
                | 0xE0000..=0xE007F
                | 0xE0100..=0xE01EF
        )
}

fn tokens(text: &str) -> Vec<String> {
    text.chars().map(token).collect()
}

fn head(tokens: &[String], limit: usize) -> (String, bool) {
    let mut used = 0;
    let mut out = String::new();
    for token in tokens {
        let size = token.chars().count();
        if used + size > limit {
            return (out, true);
        }
        used += size;
        out.push_str(token);
    }
    (out, false)
}

fn tail(tokens: &[String], limit: usize) -> (String, bool) {
    let mut used = 0;
    let mut kept: Vec<&str> = Vec::new();
    for token in tokens.iter().rev() {
        let size = token.chars().count();
        if used + size > limit {
            kept.reverse();
            return (kept.concat(), true);
        }
        used += size;
        kept.push(token);
    }
    kept.reverse();
    (kept.concat(), false)
}

pub(crate) fn excerpt(context: &str, at: &Range<usize>, shown: &str) -> String {
    let shown = shown
        .char_indices()
        .nth(SHOWN_CHARS)
        .map_or(shown, |(end, _)| shown.get(..end).unwrap_or(shown));
    let before_text = context.get(..at.start).unwrap_or_default();
    let after_text = context.get(at.end..).unwrap_or_default();
    let before_from =
        before_text.ceil_char_boundary(before_text.len().saturating_sub(CONTEXT_BYTES));
    let after_to = after_text.floor_char_boundary(CONTEXT_BYTES);
    let note = spelled(shown).map_or_else(String::new, |text| {
        let all: Vec<String> = text.chars().map(|c| c.to_string()).collect();
        let (text, cut) = head(&all, SPELLED_CHARS);
        format!(" spells \"{text}{}\"", if cut { "\u{2026}" } else { "" })
    });
    let limit = if note.is_empty() {
        SPAN_CHARS
    } else {
        SPAN_CHARS_WITH_NOTE
    };
    let span_tokens = tokens(shown);
    let (span, cut) = head(&span_tokens, limit);
    let span = if cut {
        format!("{}\u{2026}", head(&span_tokens, limit - 1).0)
    } else {
        span
    };
    let spare = EXCERPT_CHARS - MARKERS - span.chars().count() - note.chars().count();
    let side = spare.saturating_sub(2) / 2;
    let (before, before_cut) = tail(
        &tokens(before_text.get(before_from..).unwrap_or_default()),
        side,
    );
    let (after, after_cut) = head(
        &tokens(after_text.get(..after_to).unwrap_or_default()),
        side,
    );
    format!(
        "{}{before}>>{span}<<{after}{}{note}",
        if before_cut || before_from > 0 {
            "\u{2026}"
        } else {
            ""
        },
        if after_cut || after_to < after_text.len() {
            "\u{2026}"
        } else {
            ""
        },
    )
}

fn spelled(shown: &str) -> Option<String> {
    let text: String = shown
        .chars()
        .filter_map(|c| match u32::from(c) {
            code @ 0xE0020..=0xE007E => char::from_u32(code - 0xE0000),
            _ => None,
        })
        .collect();
    (!text.is_empty()).then_some(text)
}

fn clipped(text: &str, limit: usize) -> String {
    let all = tokens(text);
    let (shown, cut) = head(&all, limit);
    if cut {
        format!("{shown}\u{2026}")
    } else {
        shown
    }
}

fn push_line(out: &mut String, line: fmt::Arguments<'_>) {
    out.push_str(&line.to_string());
    out.push('\n');
}

struct Row<'a> {
    first: &'a Finding,
    count: usize,
}

impl Checked {
    #[must_use]
    pub fn report(&self, bundle: &SessionBundle) -> String {
        let mut out = String::new();
        for index in self.flagged() {
            let title = self
                .shown(bundle, index)
                .and_then(|session| session.batch.title.as_deref())
                .map_or_else(
                    || "untitled".to_owned(),
                    |title| clipped(title, TITLE_CHARS),
                );
            let found: Vec<&Finding> = self.of_session(index).collect();
            push_line(
                &mut out,
                format_args!("session {} \"{title}\": {}", index + 1, noun(found.len())),
            );
            push_rows(&mut out, &found);
        }
        let manifest: Vec<&Finding> = self.of_manifest().collect();
        if !manifest.is_empty() {
            push_line(
                &mut out,
                format_args!("bundle author fields: {}", noun(manifest.len())),
            );
            push_rows(&mut out, &manifest);
        }
        out
    }
}

fn noun(count: usize) -> String {
    if count == 1 {
        "1 finding".to_owned()
    } else {
        format!("{count} findings")
    }
}

fn push_rows(out: &mut String, found: &[&Finding]) {
    let mut rows: Vec<Row<'_>> = Vec::new();
    for finding in found {
        match rows.iter_mut().find(|row| {
            row.first.event_id == finding.event_id
                && row.first.field == finding.field
                && row.first.rule == finding.rule
        }) {
            Some(row) => row.count += 1,
            None => rows.push(Row {
                first: finding,
                count: 1,
            }),
        }
    }
    let hidden_rows = rows.len().saturating_sub(ROWS_PER_SESSION);
    for row in rows.iter().take(ROWS_PER_SESSION) {
        let finding = row.first;
        let event = finding
            .event_id
            .as_deref()
            .map_or_else(String::new, |id| format!("event {}, ", clipped(id, 40)));
        let times = if row.count > 1 {
            format!(" \u{d7}{}", row.count)
        } else {
            String::new()
        };
        let family = match finding.family {
            Family::Secret => "secret",
            Family::Injection(_) => "injection",
        };
        push_line(
            out,
            format_args!(
                "  {event}field {}, {family} rule {} ({}){times}\n    {}",
                visible(&finding.field.to_string()),
                finding.rule,
                finding.family,
                finding.excerpt
            ),
        );
    }
    if hidden_rows > 0 {
        push_line(
            out,
            format_args!("  and {hidden_rows} more rows of findings"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_excerpt_of_a_huge_span_is_cut_from_its_head() {
        let span = "a".repeat(8 << 20);
        let context = format!("before {span} after");
        let at = 7..7 + span.len();
        let shown = excerpt(&context, &at, &span);
        assert!(shown.chars().count() <= EXCERPT_CHARS);
        assert!(shown.contains(">>aaaa"));
        assert!(shown.contains("\u{2026}<<"));
    }
}
