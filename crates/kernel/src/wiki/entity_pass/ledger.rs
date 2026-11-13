use std::collections::BTreeMap;

use chrono::DateTime;
use chrono::NaiveDate;
use chrono::Utc;
use uuid::Uuid;

use super::types::FactRecord;
use super::types::LeafClock;

#[must_use]
pub fn integrate(rows: &[FactRecord], incoming: FactRecord) -> Vec<FactRecord> {
    if rows.iter().any(|row| {
        row.entity == incoming.entity
            && row.attribute == incoming.attribute
            && row.summary == incoming.summary
    }) {
        return rows.to_vec();
    }
    let mut next = rows.to_vec();
    next.push(incoming);
    recompute(&mut next);
    next
}

pub fn recompute(rows: &mut [FactRecord]) {
    let mut groups: BTreeMap<(Uuid, String), Vec<usize>> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        groups
            .entry((row.entity, row.attribute.clone()))
            .or_default()
            .push(index);
    }
    for indices in groups.values() {
        let winner = indices
            .iter()
            .copied()
            .max_by_key(|index| rows.get(*index).map(fact_rank));
        let Some(winner) = winner else {
            continue;
        };
        let Some(winner_id) = rows.get(winner).map(|row| row.id) else {
            continue;
        };
        for index in indices {
            if let Some(row) = rows.get_mut(*index) {
                row.superseded_by = (row.id != winner_id).then_some(winner_id);
            }
        }
    }
}

fn fact_rank(row: &FactRecord) -> (DateTime<Utc>, Uuid, Uuid) {
    (row.as_of, row.summary, row.id)
}

#[must_use]
pub fn place_as_of(raw: &str, leaves: &[LeafClock]) -> DateTime<Utc> {
    let Some(fallback) = leaves.last().map(|leaf| leaf.start) else {
        return DateTime::UNIX_EPOCH;
    };
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw.trim()) {
        let time = parsed.with_timezone(&Utc);
        if leaves
            .iter()
            .any(|leaf| time >= leaf.start && time <= leaf.end)
        {
            return time;
        }
    }
    let prefix = raw.trim().get(..10);
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a stamp that is not a date keeps the leaf time"
        )
    )]
    let day = prefix.and_then(|text| NaiveDate::parse_from_str(text, "%Y-%m-%d").ok());
    if let Some(day) = day
        && let Some(leaf) = leaves
            .iter()
            .find(|leaf| leaf.start.date_naive() == day || leaf.end.date_naive() == day)
    {
        return leaf.start;
    }
    fallback
}
