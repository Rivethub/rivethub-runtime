use std::collections::BTreeMap;

use chrono::Datelike as _;
use chrono::NaiveDate;
use chrono::TimeZone;

use super::Change;
use super::ChangeKind;
use super::Subject;
use super::SubjectKey;

pub const RECENTLY_UPDATED: usize = 4;
const TOP_SUBJECTS: usize = 3;
const LEVELS: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityDay {
    pub date: NaiveDate,
    pub level: u8,
}

#[must_use]
pub fn learned(changes: &[Change]) -> usize {
    changes
        .iter()
        .filter(|change| change.kind.learned())
        .count()
}

#[must_use]
pub fn headline(learned: usize, day: NaiveDate, today: NaiveDate) -> String {
    if learned == 0 {
        return "A quiet day".to_owned();
    }
    let things = if learned == 1 { "thing" } else { "things" };
    let when = if day == today {
        "today".to_owned()
    } else if today.pred_opt() == Some(day) {
        "yesterday".to_owned()
    } else {
        day.format("on %A, %B %-d").to_string()
    };
    format!("Your agents learned {learned} {things} {when}")
}

#[must_use]
pub fn fallback_summary(changes: &[Change], sessions: usize) -> String {
    if changes.is_empty() {
        return match sessions {
            0 => "Nothing was captured on this day.".to_owned(),
            1 => "1 session was captured. Nothing new was learned from it yet.".to_owned(),
            many => {
                format!("{many} sessions were captured. Nothing new was learned from them yet.")
            }
        };
    }
    let links: Vec<String> = top_subjects(changes)
        .iter()
        .filter_map(Subject::link)
        .collect();
    let counts = sentence_list(&kind_counts(changes));
    if links.is_empty() {
        format!("The day brought {counts}.")
    } else {
        format!(
            "Most of the work was on {}. The day brought {counts}.",
            sentence_list(&links)
        )
    }
}

#[must_use]
pub fn recently_updated(changes: &[Change]) -> Vec<Subject> {
    let mut seen: Vec<SubjectKey> = Vec::new();
    let mut subjects = Vec::new();
    for change in changes {
        if matches!(change.subject, Subject::Session(_)) {
            continue;
        }
        let key = change.subject.key();
        if !seen.contains(&key) {
            seen.push(key);
            subjects.push(change.subject.clone());
        }
        if subjects.len() == RECENTLY_UPDATED {
            break;
        }
    }
    subjects
}

#[must_use]
pub fn activity<Tz: TimeZone>(changes: &[Change], month: NaiveDate, zone: &Tz) -> Vec<ActivityDay> {
    let mut counts: BTreeMap<NaiveDate, usize> = BTreeMap::new();
    for change in changes {
        *counts
            .entry(change.at.with_timezone(zone).date_naive())
            .or_default() += 1;
    }
    let days: Vec<NaiveDate> = month
        .with_day(1)
        .into_iter()
        .flat_map(|first| first.iter_days())
        .take_while(|day| day.month() == month.month())
        .collect();
    let busiest = days.iter().filter_map(|day| counts.get(day)).max().copied();
    days.into_iter()
        .map(|date| ActivityDay {
            date,
            level: level(counts.get(&date).copied(), busiest),
        })
        .collect()
}

fn level(count: Option<usize>, busiest: Option<usize>) -> u8 {
    let (Some(count), Some(busiest)) = (count, busiest) else {
        return 0;
    };
    let scaled = (count * usize::from(LEVELS)).div_ceil(busiest);
    (1..LEVELS)
        .find(|level| usize::from(*level) >= scaled)
        .unwrap_or(LEVELS)
}

fn top_subjects(changes: &[Change]) -> Vec<Subject> {
    let mut tally: Vec<(Subject, usize, usize)> = Vec::new();
    for (order, change) in changes.iter().enumerate() {
        if matches!(change.subject, Subject::Session(_)) {
            continue;
        }
        match tally
            .iter_mut()
            .find(|(subject, _, _)| subject.key() == change.subject.key())
        {
            Some((_, count, _)) => *count += 1,
            None => tally.push((change.subject.clone(), 1, order)),
        }
    }
    tally.sort_by(|left, right| right.1.cmp(&left.1).then(left.2.cmp(&right.2)));
    tally
        .into_iter()
        .take(TOP_SUBJECTS)
        .map(|(subject, _, _)| subject)
        .collect()
}

fn kind_counts(changes: &[Change]) -> Vec<String> {
    let mut counts: BTreeMap<ChangeKind, usize> = BTreeMap::new();
    for change in changes {
        *counts.entry(change.kind).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(kind, count)| {
            let (one, many) = match kind {
                ChangeKind::Fact => ("fact", "facts"),
                ChangeKind::Decision => ("decision", "decisions"),
                ChangeKind::New => ("new page", "new pages"),
                ChangeKind::Filed => ("session filed", "sessions filed"),
                ChangeKind::Conflict => ("conflict", "conflicts"),
                ChangeKind::Tag => ("tag", "tags"),
                ChangeKind::Merge => ("merge", "merges"),
            };
            format!("{count} {}", if count == 1 { one } else { many })
        })
        .collect()
}

fn sentence_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}
