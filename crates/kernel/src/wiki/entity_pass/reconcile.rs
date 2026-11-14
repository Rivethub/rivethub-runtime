use std::collections::BTreeMap;

use uuid::Uuid;

use super::ledger::place_as_of;
use super::types::CONTRADICTED;
use super::types::EntityRewrite;
use super::types::FactReconciliation;
use super::types::FactState;
use super::types::HISTORY_CHARS;
use super::types::LeafClock;
use super::types::LiveFact;
use super::types::PreparedFact;
use super::types::RENAMED;
use super::types::RejectedRetirement;
use super::types::RetiredFact;
use super::types::RewriteFact;

const QUOTE_CHARS: usize = 200;

#[must_use]
pub fn snapshot_facts(
    type_name: &str,
    facts: &[RewriteFact],
    summary_id: Uuid,
    conversation_id: Option<Uuid>,
    leaves: &[LeafClock],
) -> Vec<PreparedFact> {
    let mut prepared: Vec<PreparedFact> = Vec::new();
    for fact in facts {
        let Some(next) = one_snapshot_fact(type_name, fact, summary_id, conversation_id, leaves)
        else {
            continue;
        };
        if let Some(existing) = prepared
            .iter_mut()
            .find(|item| item.attribute == next.attribute)
        {
            if next.as_of >= existing.as_of {
                *existing = next;
            }
        } else {
            prepared.push(next);
        }
    }
    prepared
}

fn one_snapshot_fact(
    type_name: &str,
    fact: &RewriteFact,
    summary_id: Uuid,
    conversation_id: Option<Uuid>,
    leaves: &[LeafClock],
) -> Option<PreparedFact> {
    let attribute = super::super::entity_vocab::canonical_attribute(type_name, &fact.attribute)?;
    let value = fact.value.trim();
    if value.is_empty() {
        return None;
    }
    Some(PreparedFact {
        attribute,
        value: value.to_owned(),
        as_of: place_as_of(&fact.as_of, leaves),
        status: FactState::Current,
        summary_id,
        conversation_id,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "a contradicted retirement is checked against this session's passages"
)]
#[must_use]
pub fn reconcile_facts(
    type_name: &str,
    page: &[LiveFact],
    rewrite: &EntityRewrite,
    summary_id: Uuid,
    conversation_id: Option<Uuid>,
    leaves: &[LeafClock],
    passages: &str,
) -> FactReconciliation {
    let mut facts = snapshot_facts(
        type_name,
        &rewrite.current_facts,
        summary_id,
        conversation_id,
        leaves,
    );
    let (accepted, rejected) = classify_retirements(type_name, &facts, &rewrite.retired, passages);
    let mut carried: Vec<PreparedFact> = Vec::new();
    let mut retired: Vec<RetiredFact> = Vec::new();
    for live in page {
        let Some(key) = stored_attribute(type_name, &live.attribute) else {
            continue;
        };
        if has_attribute(&facts, &key) {
            continue;
        }
        if let Some(item) = accepted.get(&key) {
            if retired.iter().any(|existing| existing.attribute == key) {
                continue;
            }
            retired.push(item.clone());
            continue;
        }
        let prepared = PreparedFact {
            attribute: key,
            value: live.value.clone(),
            as_of: live.as_of,
            status: live.status,
            summary_id,
            conversation_id,
        };
        facts.push(prepared.clone());
        carried.push(prepared);
    }
    let history_sentence = history_with_retirements(&rewrite.history_sentence, &retired);
    FactReconciliation {
        facts,
        history_sentence,
        carried,
        retired,
        rejected,
    }
}

fn classify_retirements(
    type_name: &str,
    facts: &[PreparedFact],
    retired: &[RetiredFact],
    passages: &str,
) -> (BTreeMap<String, RetiredFact>, Vec<RejectedRetirement>) {
    let normalized = normalize_evidence(passages);
    let mut accepted = BTreeMap::new();
    let mut rejected = Vec::new();
    for item in retired {
        let kind = item.kind.trim();
        let detail = item.detail.trim();
        let attribute = stored_attribute(type_name, &item.attribute)
            .unwrap_or_else(|| item.attribute.trim().to_owned());
        let Some(retirement) =
            accept_retirement(type_name, facts, &attribute, kind, detail, &normalized)
        else {
            rejected.push(RejectedRetirement {
                attribute,
                kind: kind.to_owned(),
                detail: detail.to_owned(),
            });
            continue;
        };
        if has_attribute(facts, &retirement.attribute) {
            continue;
        }
        accepted
            .entry(retirement.attribute.clone())
            .or_insert(retirement);
    }
    (accepted, rejected)
}

fn accept_retirement(
    type_name: &str,
    facts: &[PreparedFact],
    attribute: &str,
    kind: &str,
    detail: &str,
    passages: &str,
) -> Option<RetiredFact> {
    if attribute.is_empty() {
        return None;
    }
    let kind = canonical_kind(kind)?;
    let detail = if kind == RENAMED {
        renamed_target(type_name, facts, detail)?
    } else {
        contradicted_quote(detail, passages)?
    };
    Some(RetiredFact {
        attribute: attribute.to_owned(),
        kind: kind.to_owned(),
        detail,
    })
}

fn canonical_kind(kind: &str) -> Option<&'static str> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            dispatch,
            reason = "the model sends a free-text tag, checked against the known words"
        )
    )]
    match kind.trim().to_ascii_lowercase().as_str() {
        RENAMED => Some(RENAMED),
        CONTRADICTED => Some(CONTRADICTED),
        _ => None,
    }
}

fn renamed_target(type_name: &str, facts: &[PreparedFact], detail: &str) -> Option<String> {
    let key = stored_attribute(type_name, detail)?;
    has_attribute(facts, &key).then_some(key)
}

fn contradicted_quote(detail: &str, passages: &str) -> Option<String> {
    let quote = detail.trim();
    if quote.is_empty() || quote.chars().count() > QUOTE_CHARS {
        return None;
    }
    let needle = normalize_evidence(quote);
    if needle.is_empty() || !passages.contains(&needle) {
        return None;
    }
    Some(quote.to_owned())
}

#[must_use]
pub fn normalize_evidence(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut previous = None;
    let mut spaced = false;
    while let Some(ch) = chars.next() {
        let upcoming = chars.peek().copied();
        if is_emphasis(ch, previous, upcoming) {
            previous = Some(ch);
            continue;
        }
        previous = Some(ch);
        if ch.is_whitespace() || is_markdown_break(ch) {
            spaced = !out.is_empty();
            continue;
        }
        if spaced {
            out.push(' ');
            spaced = false;
        }
        out.push(ch);
    }
    out
}

fn is_emphasis(ch: char, previous: Option<char>, upcoming: Option<char>) -> bool {
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

fn is_markdown_break(ch: char) -> bool {
    matches!(ch, '#' | '>' | '|' | '[' | ']' | '(' | ')' | '<')
}

fn stored_attribute(type_name: &str, raw: &str) -> Option<String> {
    let key = super::super::entity_vocab::canonical_attribute(type_name, raw)
        .unwrap_or_else(|| raw.trim().to_owned());
    (!key.is_empty()).then_some(key)
}

fn has_attribute(facts: &[PreparedFact], key: &str) -> bool {
    facts.iter().any(|fact| fact.attribute == key)
}

fn history_with_retirements(sentence: &str, retired: &[RetiredFact]) -> String {
    let mut parts = Vec::new();
    let sentence = sentence.trim();
    if !sentence.is_empty() {
        parts.push(sentence.to_owned());
    }
    for item in retired {
        parts.push(retirement_line(&item.attribute, &item.reason()));
    }
    loop {
        let joined = parts.join(" ");
        if parts.len() <= 1 || joined.chars().count() <= HISTORY_CHARS {
            return joined;
        }
        parts.remove(0);
    }
}

fn retirement_line(attribute: &str, reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        format!("{attribute} retired")
    } else {
        format!("{attribute} retired: {reason}")
    }
}
