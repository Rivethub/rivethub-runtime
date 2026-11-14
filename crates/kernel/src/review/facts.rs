use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;

use super::ReviewKey;
use crate::facts::Claim;
use crate::facts::Evidence;
use crate::facts::FactState;
use crate::facts::Said;
use crate::facts::Support;
use crate::facts::facts_as_of;

pub const FACT_NEW_FOR: TimeDelta = TimeDelta::days(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    pub support: Support,
    pub words: Option<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactReview {
    pub key: ReviewKey,
    pub state: FactState,
    pub quotes: Vec<Quote>,
    pub at: DateTime<Utc>,
}

#[must_use]
pub fn fact_reviews(claims: &[Claim], now: DateTime<Utc>) -> Vec<FactReview> {
    facts_as_of(claims, None)
        .into_iter()
        .filter_map(|state| review(claims, state, now))
        .collect()
}

fn review(claims: &[Claim], state: FactState, now: DateTime<Utc>) -> Option<FactReview> {
    let history: Vec<&Claim> = claims
        .iter()
        .filter(|claim| claim.fact == state.id)
        .collect();
    let at = history.iter().map(|claim| claim.recorded_at).max()?;
    let version = at.timestamp_millis();
    if !state.candidates.is_empty() {
        let quotes = quotes(&history, &state.candidates);
        return Some(FactReview {
            key: ReviewKey::Conflict {
                fact: state.id,
                version,
            },
            state,
            quotes,
            at,
        });
    }
    let newest = history.iter().max_by(|left, right| {
        left.at
            .cmp(&right.at)
            .then(left.recorded_at.cmp(&right.recorded_at))
    })?;
    let yours = history
        .iter()
        .any(|claim| matches!(claim.said, Said::You { .. }) && claim.value == state.value);
    let fresh = now.signed_duration_since(newest.recorded_at) < FACT_NEW_FOR;
    if state.pinned || yours || !fresh || newest.said.session().is_none() || newest.said.settled() {
        return None;
    }
    let quotes = quotes(&history, &state.sources);
    Some(FactReview {
        key: ReviewKey::Fact {
            fact: state.id,
            version,
        },
        state,
        quotes,
        at,
    })
}

fn quotes(history: &[&Claim], supports: &[Support]) -> Vec<Quote> {
    supports
        .iter()
        .map(|support| Quote {
            support: support.clone(),
            words: words(history, support),
        })
        .collect()
}

fn words(history: &[&Claim], support: &Support) -> Option<Evidence> {
    history.iter().find_map(|claim| match &claim.said {
        Said::Writer {
            session: from,
            evidence,
            ..
        } if *from == support.session && claim.at == support.at && claim.value == support.value => {
            evidence.clone()
        }
        Said::Writer { .. } | Said::You { .. } => None,
    })
}
