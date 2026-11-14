use super::shape::Kept;
use super::shape::Tally;
use super::shape::Unit;

#[derive(Debug, Clone, Copy, Default)]
pub(in crate::entity_builder) struct PassageReport {
    pub(in crate::entity_builder) passage_chars: u64,
    pub(in crate::entity_builder) excerpted_messages: u64,
    pub(in crate::entity_builder) excerpt_chars: u64,
    pub(in crate::entity_builder) excerpt_source_chars: u64,
}

static PASSAGES: parking_lot::Mutex<PassageReport> = parking_lot::Mutex::new(PassageReport {
    passage_chars: 0,
    excerpted_messages: 0,
    excerpt_chars: 0,
    excerpt_source_chars: 0,
});

#[must_use]
pub(in crate::entity_builder) fn passage_report() -> PassageReport {
    *PASSAGES.lock()
}

pub(super) fn unit_of(call: Kept, result: Option<Kept>) -> Unit {
    let mut parts = vec![call];
    if let Some(result) = result {
        parts.push(result);
    }
    let mut text = String::new();
    let mut messages = 0_u64;
    let mut excerpt_chars = 0_u64;
    let mut source_chars = 0_u64;
    for part in &parts {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&part.text);
        if part.excerpted {
            messages = messages.saturating_add(1);
            excerpt_chars = excerpt_chars.saturating_add(as_u64(part.text.chars().count()));
            source_chars = source_chars.saturating_add(as_u64(part.source_chars));
        }
    }
    let chars = text.chars().count();
    Unit {
        text,
        chars,
        messages,
        excerpt_chars,
        source_chars,
    }
}

pub(super) fn cap_passages(units: &[Unit], budget: usize) -> (String, Tally) {
    let mut chosen: Vec<&Unit> = Vec::new();
    for unit in units.iter().rev() {
        let separator = usize::from(!chosen.is_empty()).saturating_mul(2);
        let used = text_len(&chosen);
        if used.saturating_add(separator).saturating_add(unit.chars) <= budget {
            chosen.insert(0, unit);
            continue;
        }
        if chosen.is_empty() {
            let text = tail(&unit.text, budget);
            let tally = tally_of(std::slice::from_ref(&unit), &text);
            return (text, tally);
        }
        break;
    }
    let text = join_passages(&chosen);
    let tally = tally_of(&chosen, &text);
    (text, tally)
}

fn text_len(parts: &[&Unit]) -> usize {
    let body: usize = parts.iter().map(|part| part.chars).sum();
    body.saturating_add(parts.len().saturating_sub(1).saturating_mul(2))
}

fn join_passages(parts: &[&Unit]) -> String {
    let mut out = String::new();
    for part in parts {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&part.text);
    }
    out
}

fn tally_of(units: &[&Unit], sent: &str) -> Tally {
    let mut tally = Tally {
        sent: as_u64(sent.chars().count()),
        messages: 0,
        excerpt_chars: 0,
        source_chars: 0,
    };
    for unit in units {
        tally.messages = tally.messages.saturating_add(unit.messages);
        tally.excerpt_chars = tally.excerpt_chars.saturating_add(unit.excerpt_chars);
        tally.source_chars = tally.source_chars.saturating_add(unit.source_chars);
    }
    tally
}

fn as_u64(len: usize) -> u64 {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a passage length that does not fit saturates the tally"
        )
    )]
    let len = u64::try_from(len).unwrap_or(u64::MAX);
    len
}

pub(super) fn record(tally: &Tally) {
    let mut stats = PASSAGES.lock();
    stats.passage_chars = stats.passage_chars.saturating_add(tally.sent);
    stats.excerpted_messages = stats.excerpted_messages.saturating_add(tally.messages);
    stats.excerpt_chars = stats.excerpt_chars.saturating_add(tally.excerpt_chars);
    stats.excerpt_source_chars = stats
        .excerpt_source_chars
        .saturating_add(tally.source_chars);
}

fn tail(text: &str, budget: usize) -> String {
    let count = text.chars().count();
    if count <= budget {
        return text.to_owned();
    }
    text.chars().skip(count.saturating_sub(budget)).collect()
}
