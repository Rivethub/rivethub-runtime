use std::collections::BTreeMap;

use ::values::SessionId;
use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;

use super::Claim;
use super::FactId;
use super::FactKey;
use super::FactValue;
use super::Said;
use super::SessionLink;
use crate::topics::TopicId;

pub const CONFLICT_WINDOW: TimeDelta = TimeDelta::hours(24);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Support {
    pub session: SessionId,
    pub at: DateTime<Utc>,
    pub value: FactValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Previous {
    pub value: FactValue,
    pub until: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactState {
    pub id: FactId,
    pub key: FactKey,
    pub value: FactValue,
    pub since: DateTime<Utc>,
    pub pinned: bool,
    pub sources: Vec<Support>,
    pub candidates: Vec<Support>,
    pub previous: Option<Previous>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactChange {
    Became {
        value: FactValue,
        session: Option<SessionId>,
    },
    Pinned {
        value: FactValue,
    },
    Disputed {
        values: Vec<FactValue>,
        session: Option<SessionId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactEvent {
    pub at: DateTime<Utc>,
    pub key: FactKey,
    pub change: FactChange,
}

#[must_use]
pub fn facts_as_of(claims: &[Claim], as_of: Option<DateTime<Utc>>) -> Vec<FactState> {
    let mut facts: Vec<FactState> = by_fact(claims, as_of)
        .into_values()
        .filter_map(|history| derive(&history))
        .collect();
    facts.sort_by(|left, right| left.key.cmp(&right.key));
    facts
}

#[must_use]
pub fn timeline(claims: &[Claim], as_of: Option<DateTime<Utc>>) -> Vec<FactEvent> {
    let mut events = Vec::new();
    for history in by_fact(claims, as_of).into_values() {
        let mut before: Option<FactState> = None;
        for (index, claim) in history.iter().enumerate() {
            let (prefix, _) = history.split_at(index + 1);
            if let Some(now) = derive(prefix) {
                events.extend(change(before.as_ref(), &now, claim));
                before = Some(now);
            }
        }
    }
    events.sort_by(|left, right| right.at.cmp(&left.at).then(left.key.cmp(&right.key)));
    events
}

#[must_use]
pub fn connected(entity: TopicId, links: &[SessionLink]) -> Vec<(TopicId, usize)> {
    let own: Vec<&SessionId> = links
        .iter()
        .filter(|link| link.entity == entity)
        .map(|link| &link.session)
        .collect();
    let mut shared: BTreeMap<TopicId, usize> = BTreeMap::new();
    for link in links {
        if link.entity != entity && own.contains(&&link.session) {
            *shared.entry(link.entity).or_default() += 1;
        }
    }
    let mut ranked: Vec<(TopicId, usize)> = shared.into_iter().collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    ranked
}

fn by_fact(claims: &[Claim], as_of: Option<DateTime<Utc>>) -> BTreeMap<FactId, Vec<&Claim>> {
    let mut grouped: BTreeMap<FactId, Vec<&Claim>> = BTreeMap::new();
    for claim in claims
        .iter()
        .filter(|claim| as_of.is_none_or(|cut| claim.at <= cut))
    {
        grouped.entry(claim.fact).or_default().push(claim);
    }
    for history in grouped.values_mut() {
        history.sort_by(|left, right| {
            left.at
                .cmp(&right.at)
                .then(left.recorded_at.cmp(&right.recorded_at))
        });
    }
    grouped
}

fn derive(history: &[&Claim]) -> Option<FactState> {
    let (newest, _) = history.split_last()?;
    let base = history
        .iter()
        .rposition(|claim| matches!(claim.said, Said::You { .. }));
    let (_, tail) = history.split_at(base.map_or(0, |index| index + 1));
    let held = base
        .filter(|&index| history.get(index).is_some_and(|claim| claim.said.pinned()))
        .unwrap_or(history.len() - 1);
    let (upto, _) = history.split_at(held + 1);
    let (holder, earlier) = upto.split_last()?;
    let value = &holder.value;
    let start = earlier
        .iter()
        .rposition(|claim| claim.value != *value)
        .map_or(0, |index| index + 1);
    let (before_run, run) = history.split_at(start);
    let first = run.first()?;
    let rivals = if holder.said.pinned() {
        rivals(tail, value, |_| true)
    } else {
        rivals(tail, value, |claim| {
            claim.said.session() != newest.said.session()
                && newest.at.signed_duration_since(claim.at) < CONFLICT_WINDOW
        })
    };
    let candidates = if rivals.is_empty() {
        rivals
    } else {
        support(holder).into_iter().chain(rivals).collect()
    };
    Some(FactState {
        id: newest.fact,
        key: newest.key.clone(),
        value: value.clone(),
        since: first.at,
        pinned: holder.said.pinned(),
        sources: run
            .iter()
            .rev()
            .filter(|claim| claim.value == *value)
            .filter_map(|claim| support(claim))
            .collect(),
        candidates,
        previous: before_run.last().map(|claim| Previous {
            value: claim.value.clone(),
            until: first.at,
        }),
    })
}

fn rivals(tail: &[&Claim], value: &FactValue, counts: impl Fn(&Claim) -> bool) -> Vec<Support> {
    let mut seen: Vec<&FactValue> = Vec::new();
    let mut found = Vec::new();
    for claim in tail.iter().rev() {
        if claim.value != *value && !seen.contains(&&claim.value) && counts(claim) {
            seen.push(&claim.value);
            found.extend(support(claim));
        }
    }
    found
}

fn support(claim: &Claim) -> Option<Support> {
    claim.said.session().map(|session| Support {
        session: session.clone(),
        at: claim.at,
        value: claim.value.clone(),
    })
}

fn change(before: Option<&FactState>, now: &FactState, claim: &Claim) -> Vec<FactEvent> {
    let mut events = Vec::new();
    let event = |change| FactEvent {
        at: claim.at,
        key: now.key.clone(),
        change,
    };
    let moved = before.is_none_or(|before| before.value != now.value);
    if claim.said.pinned() && before.is_none_or(|before| moved || !before.pinned) {
        events.push(event(FactChange::Pinned {
            value: now.value.clone(),
        }));
    } else if moved {
        events.push(event(FactChange::Became {
            value: now.value.clone(),
            session: claim.said.session().cloned(),
        }));
    }
    if !now.candidates.is_empty()
        && before.is_none_or(|before| before.candidates.len() < now.candidates.len())
    {
        events.push(event(FactChange::Disputed {
            values: now
                .candidates
                .iter()
                .map(|candidate| candidate.value.clone())
                .collect(),
            session: claim.said.session().cloned(),
        }));
    }
    events
}
