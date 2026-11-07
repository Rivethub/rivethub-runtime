use std::collections::BTreeMap;

use super::Change;
use super::ChangeActor;
use super::ChangeKind;
use super::ChangeRecord;
use super::ChangeWindow;
use super::Subject;
use crate::facts::Claim;
use crate::facts::FactChange;
use crate::facts::FactEvent;
use crate::facts::FactValue;
use crate::facts::timeline;
use crate::topics::TopicId;
use crate::topics::TopicKind;

#[must_use]
pub fn changes(records: Vec<ChangeRecord>, window: &ChangeWindow) -> Vec<Change> {
    let mut claims: BTreeMap<TopicId, (String, Vec<Claim>)> = BTreeMap::new();
    let mut found = Vec::new();
    for record in records {
        match record {
            ChangeRecord::Claim {
                entity,
                title,
                claim,
            } => claims
                .entry(entity)
                .or_insert_with(|| (title, Vec::new()))
                .1
                .push(claim),
            other @ (ChangeRecord::Decision { .. }
            | ChangeRecord::Created { .. }
            | ChangeRecord::Placed { .. }
            | ChangeRecord::Tagged { .. }
            | ChangeRecord::Merged { .. }) => found.extend(change(other)),
        }
    }
    for (entity, (title, history)) in claims {
        let subject = Subject::Entity { id: entity, title };
        found.extend(
            timeline(&history, None)
                .iter()
                .map(|event| fact_change(subject.clone(), event)),
        );
    }
    found.retain(|change| window.contains(change.at) && window.concerns(change));
    found.sort_by(|left, right| {
        right
            .at
            .cmp(&left.at)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.text.cmp(&right.text))
    });
    found
}

#[must_use]
pub fn fact_change(subject: Subject, event: &FactEvent) -> Change {
    let (kind, text) = fact_text(event);
    let (actor, source) = match &event.change {
        FactChange::Became { session, .. } | FactChange::Disputed { session, .. } => {
            (said_by(session.is_some()), session.clone())
        }
        FactChange::Pinned { .. } => (ChangeActor::You, None),
    };
    Change {
        kind,
        subject,
        text,
        at: event.at,
        actor,
        source,
    }
}

#[must_use]
pub fn fact_text(event: &FactEvent) -> (ChangeKind, String) {
    let key = event.key.as_str();
    match &event.change {
        FactChange::Became { value, .. } => {
            (ChangeKind::Fact, format!("{key}: {}", value.as_str()))
        }
        FactChange::Pinned { value } => (
            ChangeKind::Fact,
            format!("You pinned {key}: {}", value.as_str()),
        ),
        FactChange::Disputed { values, .. } => (
            ChangeKind::Conflict,
            format!(
                "Sources disagree on {key}: {}",
                values
                    .iter()
                    .map(FactValue::as_str)
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
        ),
    }
}

const fn said_by(writer: bool) -> ChangeActor {
    if writer {
        ChangeActor::WikiWriter
    } else {
        ChangeActor::You
    }
}

fn change(record: ChangeRecord) -> Option<Change> {
    let change = match record {
        ChangeRecord::Claim { .. } => return None,
        ChangeRecord::Decision {
            topic,
            text,
            session,
            at,
        } => Change {
            kind: ChangeKind::Decision,
            subject: topic.into(),
            text,
            at,
            actor: ChangeActor::You,
            source: session,
        },
        ChangeRecord::Created { topic, at } => {
            let (text, actor) = match topic.kind {
                TopicKind::Entity => (
                    format!("New entity {}.", topic.title),
                    ChangeActor::WikiWriter,
                ),
                TopicKind::Work | TopicKind::Type => {
                    (format!("New topic {}.", topic.title), ChangeActor::You)
                }
            };
            Change {
                kind: ChangeKind::New,
                subject: topic.into(),
                text,
                at,
                actor,
                source: None,
            }
        }
        ChangeRecord::Placed {
            topic,
            session,
            by,
            at,
        } => Change {
            kind: ChangeKind::Filed,
            text: format!("Filed under {}.", topic.title),
            subject: topic.into(),
            at,
            actor: by.into(),
            source: Some(session),
        },
        ChangeRecord::Tagged {
            session,
            key,
            value,
            at,
        } => Change {
            kind: ChangeKind::Tag,
            subject: Subject::Session(session.clone()),
            text: format!("Tagged {key}: {value}."),
            at,
            actor: ChangeActor::You,
            source: Some(session),
        },
        ChangeRecord::Merged { into, from, at } => Change {
            kind: ChangeKind::Merge,
            text: format!("Merged {from} into {}.", into.title),
            subject: into.into(),
            at,
            actor: ChangeActor::You,
            source: None,
        },
    };
    Some(change)
}
