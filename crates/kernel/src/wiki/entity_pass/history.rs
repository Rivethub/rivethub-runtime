use chrono::NaiveDate;
use uuid::Uuid;

use super::super::page::WikiHistoryEntry;
use super::prepare::cap_chars;
use super::types::HISTORY_CHARS;
use super::types::HistoryLine;
use super::types::LiveFact;
use super::types::PreparedHistory;

#[must_use]
pub fn render_history(lines: &[HistoryLine]) -> Vec<WikiHistoryEntry> {
    let mut ordered = lines.to_vec();
    ordered.sort_by(|left, right| {
        right
            .date
            .cmp(&left.date)
            .then(right.summary_id.cmp(&left.summary_id))
    });
    let mut seen = Vec::new();
    let mut entries = Vec::new();
    for line in ordered {
        if seen.contains(&line.summary_id) {
            continue;
        }
        seen.push(line.summary_id);
        let sentence = line.sentence.trim();
        if sentence.is_empty() {
            continue;
        }
        entries.push(WikiHistoryEntry {
            date: line.date,
            title: String::new(),
            body: cap_chars(sentence, HISTORY_CHARS),
        });
    }
    entries
}

#[must_use]
pub fn snapshot_of(facts: &[LiveFact], history: &[HistoryLine]) -> String {
    let mut facts: Vec<String> = facts
        .iter()
        .map(|fact| {
            format!(
                "{} {} {} {}",
                fact.attribute,
                fact.status.as_str(),
                fact.as_of.to_rfc3339(),
                fact.value
            )
        })
        .collect();
    facts.sort();
    let mut history: Vec<String> = history
        .iter()
        .map(|line| format!("{} {} {}", line.summary_id, line.date, line.sentence))
        .collect();
    history.sort();
    format!("{}\n{}", facts.join("\n"), history.join("\n"))
}

#[must_use]
pub fn session_history(
    summary_id: Uuid,
    conversation_id: Option<Uuid>,
    session_on: NaiveDate,
    sentence: &str,
) -> Option<PreparedHistory> {
    let sentence = cap_chars(sentence.trim(), HISTORY_CHARS);
    if sentence.is_empty() {
        return None;
    }
    Some(PreparedHistory {
        summary_id,
        conversation_id,
        session_on,
        sentence,
    })
}
