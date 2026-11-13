use kernel::wiki::entity_pass::TranscriptMessage;
use kernel::wiki::entity_pass::normalize_evidence;

use super::super::transcript::render_message;
use super::EXCERPT_CHARS;
use super::MIN_TERM;
use super::PASSAGE_CHAR_BUDGET;
use super::budget::cap_passages;
use super::budget::record;
use super::budget::unit_of;
use super::excerpt::excerpt;
use super::shape::Kept;
use super::shape::Unit;

enum Kind {
    Call,
    Result,
    Body,
}

pub(in crate::entity_builder) fn passages(
    messages: &[TranscriptMessage],
    name: &str,
    aliases: &[String],
    mention: &str,
) -> String {
    passages_within(messages, name, aliases, mention, PASSAGE_CHAR_BUDGET)
}

pub(super) fn passages_within(
    messages: &[TranscriptMessage],
    name: &str,
    aliases: &[String],
    mention: &str,
    budget: usize,
) -> String {
    let terms = terms(name, aliases);
    let needle = normalize_evidence(mention);
    if terms.is_empty() && needle.is_empty() {
        return String::new();
    }
    let ordered = ordered(messages);
    let kinds: Vec<Kind> = ordered.iter().map(kind_of).collect();
    let direct: Vec<bool> = ordered
        .iter()
        .map(|message| mentioned(&render_message(message), &terms, &needle))
        .collect();
    let keep = partners(&kinds, &direct);
    let (text, tally) = cap_passages(&units(&ordered, &kinds, &keep, &terms, &needle), budget);
    record(&tally);
    text
}

fn terms(name: &str, aliases: &[String]) -> Vec<String> {
    let mut terms = Vec::with_capacity(aliases.len().saturating_add(1));
    push_term(&mut terms, name);
    for alias in aliases {
        push_term(&mut terms, alias);
    }
    terms
}

fn push_term(terms: &mut Vec<String>, text: &str) {
    let term = text.trim().to_ascii_lowercase();
    if term.chars().count() >= MIN_TERM {
        terms.push(term);
    }
}

fn ordered(messages: &[TranscriptMessage]) -> Vec<TranscriptMessage> {
    let mut ordered = messages.to_vec();
    ordered.sort_by(|left, right| left.at.cmp(&right.at).then(left.id.cmp(&right.id)));
    ordered
}

fn kind_of(message: &TranscriptMessage) -> Kind {
    let has_args = message.tool_args.is_some();
    let has_result = message
        .tool_result
        .as_ref()
        .is_some_and(|text| !text.is_empty());
    if has_args && has_result {
        return Kind::Body;
    }
    if has_result || message.role.eq_ignore_ascii_case("tool") {
        return Kind::Result;
    }
    let named = message
        .tool_name
        .as_ref()
        .is_some_and(|name| !name.trim().is_empty());
    if has_args || named {
        return Kind::Call;
    }
    Kind::Body
}

pub(super) fn mentioned(text: &str, terms: &[String], mention: &str) -> bool {
    terms.iter().any(|term| term_in(text, term)) || mention_in(text, mention)
}

pub(super) fn mention_in(text: &str, mention: &str) -> bool {
    !mention.is_empty() && normalize_evidence(text).contains(mention)
}

fn term_in(haystack: &str, needle: &str) -> bool {
    let needle: Vec<char> = needle.chars().collect();
    let width = needle.len();
    if width < MIN_TERM {
        return false;
    }
    let chars: Vec<char> = haystack.chars().map(|ch| ch.to_ascii_lowercase()).collect();
    let Some(last) = chars.len().checked_sub(width) else {
        return false;
    };
    for start in 0..=last {
        if !window_eq(&chars, start, &needle) {
            continue;
        }
        let before = start
            .checked_sub(1)
            .and_then(|index| chars.get(index).copied());
        let after = chars.get(start.saturating_add(width)).copied();
        if boundary(before) && boundary(after) {
            return true;
        }
    }
    false
}

pub(super) fn window_eq(chars: &[char], start: usize, needle: &[char]) -> bool {
    needle
        .iter()
        .enumerate()
        .all(|(offset, wanted)| chars.get(start.saturating_add(offset)) == Some(wanted))
}

pub(super) fn boundary(ch: Option<char>) -> bool {
    ch.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
}

fn partners(kinds: &[Kind], direct: &[bool]) -> Vec<bool> {
    let mut keep = direct.to_vec();
    for (index, hit) in direct.iter().enumerate() {
        if !hit {
            continue;
        }
        match kinds.get(index) {
            Some(Kind::Call) => mark_result(&mut keep, kinds, index.saturating_add(1)),
            Some(Kind::Result) => mark_call(&mut keep, kinds, index.checked_sub(1)),
            Some(Kind::Body) | None => {}
        }
    }
    keep
}

fn mark_result(keep: &mut [bool], kinds: &[Kind], index: usize) {
    if matches!(kinds.get(index), Some(Kind::Result))
        && let Some(flag) = keep.get_mut(index)
    {
        *flag = true;
    }
}

fn mark_call(keep: &mut [bool], kinds: &[Kind], index: Option<usize>) {
    let Some(index) = index else {
        return;
    };
    if matches!(kinds.get(index), Some(Kind::Call))
        && let Some(flag) = keep.get_mut(index)
    {
        *flag = true;
    }
}

fn units(
    messages: &[TranscriptMessage],
    kinds: &[Kind],
    keep: &[bool],
    terms: &[String],
    mention: &str,
) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        if keep.get(index).copied() != Some(true) {
            index = index.saturating_add(1);
            continue;
        }
        let Some(message) = messages.get(index) else {
            break;
        };
        let next = index.saturating_add(1);
        let paired = matches!(kinds.get(index), Some(Kind::Call))
            && matches!(kinds.get(next), Some(Kind::Result))
            && keep.get(next).copied() == Some(true);
        if paired {
            let call = kept_message(message, terms, mention);
            let result = messages
                .get(next)
                .map(|item| kept_message(item, terms, mention));
            index = next.saturating_add(1);
            out.push(unit_of(call, result));
        } else {
            index = index.saturating_add(1);
            out.push(unit_of(kept_message(message, terms, mention), None));
        }
    }
    out
}

fn kept_message(message: &TranscriptMessage, terms: &[String], mention: &str) -> Kept {
    let text = render_message(message);
    let source_chars = text.chars().count();
    if source_chars <= EXCERPT_CHARS {
        return Kept {
            text,
            source_chars,
            excerpted: false,
        };
    }
    let Some(excerpt) = excerpt(&text, message, terms, mention) else {
        return Kept {
            text,
            source_chars,
            excerpted: false,
        };
    };
    if excerpt.chars().count() >= source_chars {
        return Kept {
            text,
            source_chars,
            excerpted: false,
        };
    }
    Kept {
        text: excerpt,
        source_chars,
        excerpted: true,
    }
}
