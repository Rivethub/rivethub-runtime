#![cfg(test)]

use chrono::DateTime;
use chrono::NaiveDate;
use chrono::Utc;
use chrono_tz::America::New_York;
use kernel::changes::Change;
use kernel::changes::ChangeActor;
use kernel::changes::ChangeKind;
use kernel::changes::DIGEST_MAX_CHARS;
use kernel::changes::Subject;
use kernel::changes::activity;
use kernel::changes::clean_digest;
use kernel::changes::fallback_summary;
use kernel::changes::headline;
use kernel::changes::learned;
use kernel::changes::recently_updated;
use kernel::topics::TopicId;

fn day(text: &str) -> NaiveDate {
    text.parse().unwrap()
}

fn instant(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn topic(id: &str, title: &str) -> Subject {
    Subject::Topic {
        id: id.parse::<TopicId>().unwrap(),
        title: title.to_owned(),
    }
}

fn entity(id: &str, title: &str) -> Subject {
    Subject::Entity {
        id: id.parse::<TopicId>().unwrap(),
        title: title.to_owned(),
    }
}

const BACKUPS: &str = "00000000-0000-4000-8000-0000000000a1";
const NAS: &str = "00000000-0000-4000-8000-0000000000a2";
const MESH: &str = "00000000-0000-4000-8000-0000000000a3";

fn change(kind: ChangeKind, subject: Subject, at: &str) -> Change {
    Change {
        kind,
        subject,
        text: "something".to_owned(),
        at: instant(at),
        actor: ChangeActor::WikiWriter,
        source: None,
    }
}

fn sample() -> Vec<Change> {
    vec![
        change(
            ChangeKind::Fact,
            entity(NAS, "nas-01"),
            "2026-11-05T20:00:00Z",
        ),
        change(
            ChangeKind::Decision,
            topic(BACKUPS, "Backups"),
            "2026-11-05T19:00:00Z",
        ),
        change(
            ChangeKind::Filed,
            topic(BACKUPS, "Backups"),
            "2026-11-05T18:00:00Z",
        ),
        change(
            ChangeKind::New,
            topic(MESH, "Mesh [lab]"),
            "2026-11-05T17:00:00Z",
        ),
        change(
            ChangeKind::Fact,
            entity(NAS, "nas-01"),
            "2026-11-05T16:00:00Z",
        ),
        change(
            ChangeKind::Fact,
            entity(NAS, "nas-01"),
            "2026-11-03T16:00:00Z",
        ),
    ]
}

#[test]
fn the_headline_counts_facts_decisions_and_new_pages() {
    let changes = sample();
    assert_eq!(learned(&changes), 5);
    assert_eq!(
        headline(5, day("2026-11-05"), day("2026-11-05")),
        "Your agents learned 5 things today"
    );
    assert_eq!(
        headline(1, day("2026-11-04"), day("2026-11-05")),
        "Your agents learned 1 thing yesterday"
    );
    assert_eq!(
        headline(3, day("2026-11-02"), day("2026-11-05")),
        "Your agents learned 3 things on Monday, November 2"
    );
    assert_eq!(
        headline(0, day("2026-11-05"), day("2026-11-05")),
        "A quiet day"
    );
}

#[test]
fn the_fallback_summary_links_the_busiest_subjects_and_counts_the_kinds() {
    let changes = sample();
    assert_eq!(
        fallback_summary(&changes, 2),
        format!(
            "Most of the work was on [nas-01](/entities/{NAS}), [Backups](/topics/{BACKUPS}) and \
             [Mesh lab](/topics/{MESH}). The day brought 3 facts, 1 decision, 1 new page and 1 session filed."
        )
    );
    assert_eq!(
        fallback_summary(&[], 0),
        "Nothing was captured on this day."
    );
    assert_eq!(
        fallback_summary(&[], 3),
        "3 sessions were captured. Nothing new was learned from them yet."
    );
}

#[test]
fn recently_updated_names_each_topic_or_entity_once_newest_first() {
    let mut changes = sample();
    changes.insert(
        0,
        change(
            ChangeKind::Tag,
            Subject::Session(
                "claude-code:00000000-0000-4000-8000-0000000000c1"
                    .parse()
                    .unwrap(),
            ),
            "2026-11-05T21:00:00Z",
        ),
    );
    assert_eq!(
        recently_updated(&changes),
        vec![
            entity(NAS, "nas-01"),
            topic(BACKUPS, "Backups"),
            topic(MESH, "Mesh [lab]")
        ]
    );
}

#[test]
fn activity_scales_each_day_of_the_month_against_the_busiest_in_the_node_zone() {
    let mut changes = sample();
    changes.push(change(
        ChangeKind::Fact,
        entity(NAS, "nas-01"),
        "2026-11-01T03:00:00Z",
    ));
    let days = activity(&changes, day("2026-11-17"), &New_York);
    assert_eq!(days.len(), 30);
    let level = |date: &str| {
        days.iter()
            .find(|entry| entry.date == day(date))
            .map(|entry| entry.level)
            .unwrap()
    };
    assert_eq!(level("2026-11-05"), 4);
    assert_eq!(level("2026-11-03"), 1);
    assert_eq!(
        level("2026-11-01"),
        0,
        "03:00 UTC is still October 31 in New York"
    );
    assert_eq!(level("2026-11-30"), 0);
}

#[test]
fn a_digest_is_one_trimmed_paragraph_cut_at_a_sentence() {
    assert_eq!(clean_digest("  \n "), None);
    assert_eq!(
        clean_digest("Backups moved.\n\nThe NAS keeps 14 days.").as_deref(),
        Some("Backups moved. The NAS keeps 14 days.")
    );
    let long = "A sentence that goes on. ".repeat(80);
    let cut = clean_digest(&long).unwrap();
    assert!(cut.chars().count() <= DIGEST_MAX_CHARS);
    assert!(cut.ends_with('.'), "{cut}");
}
