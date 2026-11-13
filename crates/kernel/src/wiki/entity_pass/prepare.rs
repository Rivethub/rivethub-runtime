use chrono::DateTime;
use chrono::Utc;

use super::bind::compact_key;
use super::ledger::place_as_of;
use super::types::EntityObservation;
use super::types::HISTORY_CHARS;
use super::types::LeafClock;
use super::types::ObservedFact;
use super::types::ObservedHistory;
use super::types::PreparedEntity;
use super::types::PreparedFact;
use super::types::PreparedHistory;

#[must_use]
pub fn merge_same(observations: Vec<EntityObservation>) -> Vec<EntityObservation> {
    let mut merged = Vec::new();
    for observation in observations {
        let name = compact_key(&observation.name);
        let kind = compact_key(&observation.type_name);
        let slot = merged.iter_mut().find(|kept: &&mut EntityObservation| {
            !name.is_empty()
                && compact_key(&kept.name) == name
                && compact_key(&kept.type_name) == kind
        });
        if let Some(kept) = slot {
            if kept.entity_ref.is_none() {
                kept.entity_ref = observation.entity_ref;
            }
            push_new(&mut kept.aliases, observation.aliases);
            if kept.mention.trim().is_empty() {
                kept.mention.clone_from(&observation.mention);
            }
            kept.facts.extend(observation.facts);
            kept.history.extend(observation.history);
        } else {
            merged.push(observation);
        }
    }
    merged
}

fn push_new(target: &mut Vec<String>, more: Vec<String>) {
    for item in more {
        if !item.trim().is_empty() && !target.iter().any(|have| have == &item) {
            target.push(item);
        }
    }
}

#[must_use]
pub fn prepare(observation: &EntityObservation, leaves: &[LeafClock]) -> Option<PreparedEntity> {
    let name = observation.name.trim();
    if name.is_empty() || leaves.is_empty() || compact_key(name).is_empty() {
        return None;
    }
    let newest = leaves.last()?;
    Some(PreparedEntity {
        entity_ref: observation.entity_ref,
        name: name.to_owned(),
        type_name: observation.type_name.trim().to_owned(),
        aliases: observation
            .aliases
            .iter()
            .map(|alias| alias.trim().to_owned())
            .filter(|alias| !alias.is_empty())
            .collect(),
        facts: prepare_facts(&observation.type_name, &observation.facts, leaves, newest),
        history: prepare_history(&observation.history, leaves, newest),
    })
}

fn prepare_facts(
    type_name: &str,
    facts: &[ObservedFact],
    leaves: &[LeafClock],
    newest: &LeafClock,
) -> Vec<PreparedFact> {
    let mut prepared: Vec<PreparedFact> = Vec::new();
    for fact in facts {
        let Some(attribute) =
            super::super::entity_vocab::canonical_attribute(type_name, &fact.attribute)
        else {
            continue;
        };
        let value = fact.value.trim();
        if value.is_empty() {
            continue;
        }
        let as_of = place_as_of(&fact.as_of, leaves);
        let leaf = leaf_for(as_of, leaves).unwrap_or(newest);
        let next = PreparedFact {
            attribute,
            value: value.to_owned(),
            as_of,
            status: fact.status,
            summary_id: leaf.id,
            conversation_id: leaf.conversation_id,
        };
        if let Some(existing) = prepared
            .iter_mut()
            .find(|item| item.attribute == next.attribute)
        {
            merge_attribute(existing, next);
        } else {
            prepared.push(next);
        }
    }
    collapse_restatements(&mut prepared);
    prepared
}

fn merge_attribute(existing: &mut PreparedFact, next: PreparedFact) {
    let incoming_wins = next.as_of >= existing.as_of;
    if compact_key(&existing.value) == compact_key(&next.value) {
        if incoming_wins {
            copy_fact_meta(existing, &next);
        }
        return;
    }
    if let Some(joined) = join_subitems(&existing.value, &next.value, incoming_wins) {
        if incoming_wins {
            copy_fact_meta(existing, &next);
        }
        existing.value = joined;
        return;
    }
    if incoming_wins {
        *existing = next;
    }
}

fn copy_fact_meta(existing: &mut PreparedFact, next: &PreparedFact) {
    existing.as_of = next.as_of;
    existing.status = next.status;
    existing.summary_id = next.summary_id;
    existing.conversation_id = next.conversation_id;
}

fn join_subitems(existing: &str, incoming: &str, incoming_wins: bool) -> Option<String> {
    let mut labeled: Vec<(String, String)> = Vec::new();
    absorb_parts(&mut labeled, existing, false)?;
    absorb_parts(&mut labeled, incoming, incoming_wins)?;
    if labeled.len() < 2 {
        return None;
    }
    labeled.sort_by(|left, right| left.0.cmp(&right.0));
    Some(
        labeled
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>()
            .join("; "),
    )
}

fn absorb_parts(labeled: &mut Vec<(String, String)>, value: &str, replace: bool) -> Option<()> {
    for piece in value.split(';') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let label = subitem_label(piece)?.to_owned();
        let key = compact_key(&label);
        if let Some(slot) = labeled.iter_mut().find(|item| item.0 == key) {
            if replace {
                piece.clone_into(&mut slot.1);
            }
        } else {
            labeled.push((key, piece.to_owned()));
        }
    }
    Some(())
}

fn subitem_label(value: &str) -> Option<&str> {
    let (label, rest) = value.split_once(':')?;
    let label = label.trim();
    let rest = rest.trim();
    if label.is_empty() || rest.is_empty() || label.chars().count() > 48 {
        return None;
    }
    if label.split_whitespace().count() > 6 {
        return None;
    }
    Some(label)
}

fn collapse_restatements(facts: &mut Vec<PreparedFact>) {
    let mut dropped = vec![false; facts.len()];
    let mut index = 0;
    while index < facts.len() {
        if dropped.get(index).copied().unwrap_or(true) {
            index = index.saturating_add(1);
            continue;
        }
        let Some(value) = facts.get(index).map(|fact| compact_key(&fact.value)) else {
            break;
        };
        if value.is_empty() {
            index = index.saturating_add(1);
            continue;
        }
        let mut group = vec![index];
        for other in index.saturating_add(1)..facts.len() {
            let same = dropped.get(other).copied() == Some(false)
                && facts
                    .get(other)
                    .is_some_and(|fact| compact_key(&fact.value) == value);
            if same {
                group.push(other);
            }
        }
        if group.len() >= 2 {
            let chosen = preferred_restatement(facts, &group);
            for member in group {
                if member != chosen
                    && let Some(flag) = dropped.get_mut(member)
                {
                    *flag = true;
                }
            }
        }
        index = index.saturating_add(1);
    }
    let mut cursor = 0;
    facts.retain(|_| {
        let held = dropped.get(cursor).copied() != Some(true);
        cursor = cursor.saturating_add(1);
        held
    });
}

fn preferred_restatement(facts: &[PreparedFact], group: &[usize]) -> usize {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            strset,
            reason = "free-text vocabulary matched against transcripts, not a protocol enum"
        )
    )]
    const WEAK: &[&str] = &["role", "location", "state", "identity"];
    let Some(first) = group.first().copied() else {
        return 0;
    };
    let sample = facts
        .get(first)
        .map(|fact| fact.value.to_lowercase())
        .unwrap_or_default();
    if sample.contains("gpu")
        && let Some(found) = group.iter().copied().find(|index| {
            facts
                .get(*index)
                .is_some_and(|fact| fact.attribute == "gpus")
        })
    {
        return found;
    }
    if sample.contains("service")
        && let Some(found) = group.iter().copied().find(|index| {
            facts
                .get(*index)
                .is_some_and(|fact| fact.attribute == "services")
        })
    {
        return found;
    }
    if let Some(found) = group.iter().copied().find(|index| {
        facts
            .get(*index)
            .is_some_and(|fact| !WEAK.contains(&fact.attribute.as_str()))
    }) {
        return found;
    }
    group
        .iter()
        .copied()
        .max_by_key(|index| facts.get(*index).map(|fact| fact.as_of))
        .unwrap_or(first)
}

fn prepare_history(
    history: &[ObservedHistory],
    leaves: &[LeafClock],
    newest: &LeafClock,
) -> Vec<PreparedHistory> {
    let mut prepared: Vec<PreparedHistory> = Vec::new();
    for item in history {
        let sentence = cap_chars(item.sentence.trim(), HISTORY_CHARS);
        if sentence.is_empty() {
            continue;
        }
        let leaf = item
            .summary_id
            .and_then(|id| leaves.iter().find(|leaf| leaf.id == id))
            .unwrap_or(newest);
        let next = PreparedHistory {
            summary_id: leaf.id,
            conversation_id: leaf.conversation_id,
            session_on: leaf.start.date_naive(),
            sentence,
        };
        if let Some(existing) = prepared
            .iter_mut()
            .find(|item| item.summary_id == next.summary_id)
        {
            *existing = next;
        } else {
            prepared.push(next);
        }
    }
    prepared
}

fn leaf_for(time: DateTime<Utc>, leaves: &[LeafClock]) -> Option<&LeafClock> {
    leaves
        .iter()
        .find(|leaf| time >= leaf.start && time <= leaf.end)
}

pub(super) fn cap_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}
