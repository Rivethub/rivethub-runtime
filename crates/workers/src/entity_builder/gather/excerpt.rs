use kernel::wiki::entity_pass::TranscriptMessage;

use super::super::transcript::call_line;
use super::CHAR_WINDOW;
use super::FEW_LONG_LINES;
use super::MIN_TERM;
use super::WINDOW_LINES;
use super::select::boundary;
use super::select::mention_in;
use super::select::mentioned;
use super::select::window_eq;
use super::shape::Piece;

pub(super) fn excerpt(
    text: &str,
    message: &TranscriptMessage,
    terms: &[String],
    mention: &str,
) -> Option<String> {
    let lines = pieces(text);
    if lines.is_empty() {
        return None;
    }
    if long_line_dump(&lines) {
        return char_excerpt(text, &lines, message, terms, mention);
    }
    let windows = merge_ranges(line_windows(
        &hit_lines(&lines, terms, mention),
        lines.len(),
    ));
    if covers_most(&windows, lines.len()) {
        return None;
    }
    let call = call_line(message);
    let forced = forced_lines(&lines, call.as_deref());
    let ranges = merge_ranges(extend(forced, windows));
    Some(stitch_lines(&lines, &ranges))
}

fn long_line_dump(lines: &[Piece]) -> bool {
    if lines.is_empty() || lines.len() > FEW_LONG_LINES {
        return false;
    }
    lines
        .iter()
        .any(|piece| piece.line.chars().count() > CHAR_WINDOW)
}

fn pieces(text: &str) -> Vec<Piece> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut start = 0_usize;
    let mut count = 0_usize;
    for ch in text.chars() {
        count = count.saturating_add(1);
        if ch == '\n' {
            lines.push(Piece {
                line: std::mem::take(&mut line),
                start,
                end: count,
            });
            start = count;
            continue;
        }
        line.push(ch);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(Piece {
            line,
            start,
            end: count,
        });
    }
    lines
}

fn hit_lines(lines: &[Piece], terms: &[String], mention: &str) -> Vec<usize> {
    let mut hits = Vec::new();
    for (index, piece) in lines.iter().enumerate() {
        if mentioned(&piece.line, terms, mention) {
            hits.push(index);
        }
    }
    if mention.is_empty() {
        return hits;
    }
    pair_mentions(lines, mention, &mut hits);
    hits.sort_unstable();
    hits.dedup();
    hits
}

fn pair_mentions(lines: &[Piece], mention: &str, hits: &mut Vec<usize>) {
    let last = lines.len().saturating_sub(1);
    for index in 0..last {
        let Some(left) = lines.get(index) else {
            continue;
        };
        let Some(right) = lines.get(index.saturating_add(1)) else {
            continue;
        };
        if mention_in(&left.line, mention) || mention_in(&right.line, mention) {
            continue;
        }
        let mut pair = String::new();
        pair.push_str(&left.line);
        pair.push('\n');
        pair.push_str(&right.line);
        if mention_in(&pair, mention) {
            hits.push(index);
            hits.push(index.saturating_add(1));
        }
    }
}

fn line_windows(hits: &[usize], len: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::with_capacity(hits.len());
    for hit in hits {
        let start = hit.saturating_sub(WINDOW_LINES);
        let end = hit.saturating_add(WINDOW_LINES).saturating_add(1).min(len);
        ranges.push((start, end));
    }
    ranges
}

fn covers_most(ranges: &[(usize, usize)], total: usize) -> bool {
    total > 0 && covered_len(ranges).saturating_mul(5) > total.saturating_mul(4)
}

fn covered_len(ranges: &[(usize, usize)]) -> usize {
    ranges.iter().fold(0, |sum, (start, end)| {
        sum.saturating_add(end.saturating_sub(*start))
    })
}

fn forced_lines(lines: &[Piece], call: Option<&str>) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    if !lines.is_empty() {
        ranges.push((0, 1));
    }
    let Some(call) = call else {
        return ranges;
    };
    let call_lines: Vec<&str> = call.split('\n').collect();
    let Some(start) = find_lines(lines, &call_lines) else {
        return ranges;
    };
    let end = start.saturating_add(call_lines.len()).min(lines.len());
    ranges.push((start, end));
    ranges
}

fn find_lines(lines: &[Piece], needle: &[&str]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let last = lines.len().checked_sub(needle.len())?;
    for start in 0..=last {
        let matched = needle.iter().enumerate().all(|(offset, expected)| {
            lines
                .get(start.saturating_add(offset))
                .is_some_and(|piece| piece.line == *expected)
        });
        if matched {
            return Some(start);
        }
    }
    None
}

fn extend(mut base: Vec<(usize, usize)>, more: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    base.extend(more);
    base
}

fn merge_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.retain(|(start, end)| *end > *start);
    ranges.sort_unstable_by_key(|range| (range.0, range.1));
    let mut merged = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    for range in ranges {
        match current {
            None => current = Some(range),
            Some(open) if range.0 <= open.1 => {
                current = Some((open.0, open.1.max(range.1)));
            }
            Some(open) => {
                merged.push(open);
                current = Some(range);
            }
        }
    }
    if let Some(open) = current {
        merged.push(open);
    }
    merged
}

fn stitch_lines(lines: &[Piece], ranges: &[(usize, usize)]) -> String {
    let mut out = String::new();
    let mut cursor = 0_usize;
    for &(start, end) in ranges {
        if start > cursor {
            push_line(&mut out, &elision(start.saturating_sub(cursor), "lines"));
        }
        for piece in lines.iter().skip(start).take(end.saturating_sub(start)) {
            push_line(&mut out, &piece.line);
        }
        cursor = end.max(cursor);
    }
    if cursor < lines.len() {
        push_line(
            &mut out,
            &elision(lines.len().saturating_sub(cursor), "lines"),
        );
    }
    out
}

fn push_line(out: &mut String, line: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(line);
}

fn elision(skipped: usize, unit: &str) -> String {
    format!("[... {skipped} {unit} elided ...]")
}

fn char_excerpt(
    text: &str,
    lines: &[Piece],
    message: &TranscriptMessage,
    terms: &[String],
    mention: &str,
) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let hits = char_hits(&chars, terms, mention);
    let windows = merge_ranges(char_windows(&hits, chars.len()));
    if covers_most(&windows, chars.len()) {
        return None;
    }
    let call = call_line(message);
    let forced = forced_chars(lines, call.as_deref());
    let ranges = merge_ranges(extend(forced, windows));
    Some(stitch_chars(&chars, &ranges))
}

fn char_hits(chars: &[char], terms: &[String], mention: &str) -> Vec<(usize, usize)> {
    let mut hits = Vec::new();
    for term in terms {
        hits.extend(term_ranges(chars, term));
    }
    hits.extend(mention_ranges(chars, mention));
    hits
}

fn term_ranges(chars: &[char], needle: &str) -> Vec<(usize, usize)> {
    let needle: Vec<char> = needle.chars().collect();
    let width = needle.len();
    if width < MIN_TERM {
        return Vec::new();
    }
    let lower: Vec<char> = chars.iter().map(char::to_ascii_lowercase).collect();
    let Some(last) = lower.len().checked_sub(width) else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    for start in 0..=last {
        if !window_eq(&lower, start, &needle) {
            continue;
        }
        let before = start
            .checked_sub(1)
            .and_then(|index| lower.get(index).copied());
        let after = lower.get(start.saturating_add(width)).copied();
        if boundary(before) && boundary(after) {
            hits.push((start, start.saturating_add(width)));
        }
    }
    hits
}

fn mention_ranges(chars: &[char], mention: &str) -> Vec<(usize, usize)> {
    if mention.is_empty() {
        return Vec::new();
    }
    let (normalized, map) = normalize_map(chars);
    let needle: Vec<char> = mention.chars().collect();
    let hay: Vec<char> = normalized.chars().collect();
    let width = needle.len();
    let Some(last) = hay.len().checked_sub(width) else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    for start in 0..=last {
        if !window_eq(&hay, start, &needle) {
            continue;
        }
        let Some(from) = map.get(start).copied() else {
            continue;
        };
        let Some(to) = map
            .get(start.saturating_add(width).saturating_sub(1))
            .copied()
        else {
            continue;
        };
        hits.push((from, to.saturating_add(1)));
    }
    hits
}

pub(super) fn normalize_map(chars: &[char]) -> (String, Vec<usize>) {
    let mut out = String::new();
    let mut map = Vec::new();
    let mut previous = None;
    let mut spaced = false;
    let mut space_at = 0_usize;
    for (index, ch) in chars.iter().copied().enumerate() {
        let upcoming = chars.get(index.saturating_add(1)).copied();
        if emphasis(ch, previous, upcoming) {
            previous = Some(ch);
            continue;
        }
        previous = Some(ch);
        if ch.is_whitespace() || markdown_break(ch) {
            if !out.is_empty() {
                spaced = true;
                space_at = index;
            }
            continue;
        }
        if spaced {
            out.push(' ');
            map.push(space_at);
            spaced = false;
        }
        out.push(ch);
        map.push(index);
    }
    (out, map)
}

fn emphasis(ch: char, previous: Option<char>, upcoming: Option<char>) -> bool {
    match ch {
        '*' | '`' | '~' => true,
        '_' => {
            let prev_word = previous.is_some_and(char::is_alphanumeric);
            let next_word = upcoming.is_some_and(char::is_alphanumeric);
            !(prev_word && next_word)
        }
        _ => false,
    }
}

fn markdown_break(ch: char) -> bool {
    matches!(ch, '#' | '>' | '|' | '[' | ']' | '(' | ')' | '<')
}

fn char_windows(hits: &[(usize, usize)], total: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::with_capacity(hits.len());
    for (start, end) in hits {
        let from = start.saturating_sub(CHAR_WINDOW);
        let to = end.saturating_add(CHAR_WINDOW).min(total);
        ranges.push((from, to));
    }
    ranges
}

fn forced_chars(lines: &[Piece], call: Option<&str>) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    if let Some(first) = lines.first() {
        ranges.push((first.start, first.end));
    }
    let Some(call) = call else {
        return ranges;
    };
    let call_lines: Vec<&str> = call.split('\n').collect();
    let Some(start) = find_lines(lines, &call_lines) else {
        return ranges;
    };
    for offset in 0..call_lines.len() {
        if let Some(piece) = lines.get(start.saturating_add(offset)) {
            ranges.push((piece.start, piece.end));
        }
    }
    ranges
}

fn stitch_chars(chars: &[char], ranges: &[(usize, usize)]) -> String {
    let mut out = String::new();
    let mut cursor = 0_usize;
    for &(start, end) in ranges {
        if start > cursor {
            out.push_str(&elision(start.saturating_sub(cursor), "chars"));
        }
        for ch in chars.iter().skip(start).take(end.saturating_sub(start)) {
            out.push(*ch);
        }
        cursor = end.max(cursor);
    }
    if cursor < chars.len() {
        out.push_str(&elision(chars.len().saturating_sub(cursor), "chars"));
    }
    out
}
