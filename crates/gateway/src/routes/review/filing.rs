use kernel::review::FilingTarget;
use kernel::review::ReviewKey;
use kernel::topics::Slug;
use kernel::topics::Suggestion;
use kernel::topics::TopicId;
use kernel::topics::TopicName;
use wire::gateway::entities::SessionMention;
use wire::gateway::review::ReviewEvidence;
use wire::gateway::review::ReviewGroup;
use wire::gateway::review::ReviewItem;
use wire::gateway::review::SubjectRef;
use wire::session_id::SessionId;

use super::Action;
use super::Item;
use super::Topics;
use crate::Gateway;
use crate::error::ApiError;
use crate::routes::topics::topic_error;

const PATH_JOIN: &str = " › ";

pub(super) struct FilingAction {
    pub(super) session: SessionId,
    pub(super) destination: Destination,
}

pub(super) enum Destination {
    Topic(TopicId),
    New {
        name: TopicName,
        parent: Option<TopicId>,
    },
}

pub(super) async fn items(gateway: &Gateway, topics: &Topics) -> Result<Vec<Item>, ApiError> {
    let suggestions = gateway.topics()?.suggestions().await.map_err(topic_error)?;
    let catalog = gateway
        .session_catalog()
        .ok_or_else(|| ApiError::Unavailable("session catalog unavailable".into()))?;
    let mut items = Vec::new();
    for suggestion in suggestions {
        let Some(record) = catalog.get(&suggestion.session).await? else {
            continue;
        };
        if record.trashed || record.topic.is_some() {
            continue;
        }
        let Some((into, destination, text, after)) = destination(topics, &suggestion)? else {
            continue;
        };
        let key = ReviewKey::Filing {
            session: suggestion.session.clone(),
            into,
        };
        let mention = SessionMention {
            id: record.id.clone(),
            title: record.title.clone(),
        };
        items.push(Item {
            body: ReviewItem {
                id: key.to_string(),
                group: ReviewGroup::Filing,
                subject: SubjectRef::Session {
                    id: record.id.clone(),
                    title: record.title.clone(),
                },
                text,
                before: None,
                after: Some(after),
                options: None,
                evidence: vec![ReviewEvidence {
                    session: mention,
                    at: record.started_at,
                    quote: record.gist.clone().unwrap_or_else(|| record.title.clone()),
                    highlight: None,
                }],
                created_at: record.ended_at.unwrap_or(record.started_at),
            },
            key,
            action: Action::Filing(FilingAction {
                session: suggestion.session,
                destination,
            }),
        });
    }
    Ok(items)
}

fn destination(
    topics: &Topics,
    suggestion: &Suggestion,
) -> Result<Option<(FilingTarget, Destination, String, String)>, ApiError> {
    match suggestion.topic {
        Some(topic) if topics.get(topic).is_some() => Ok(Some((
            FilingTarget::Topic(topic),
            Destination::Topic(topic),
            format!("→ {}", topics.path(topic).join(PATH_JOIN)),
            topic.to_string(),
        ))),
        Some(_) => Ok(None),
        None => {
            let name = suggestion.name.clone();
            let mut path = suggestion
                .parent
                .map_or_else(Vec::new, |parent| topics.path(parent));
            path.push(name.as_str().to_owned());
            Ok(Some((
                FilingTarget::New(Slug::fold(name.as_str()).map_err(topic_error)?),
                Destination::New {
                    parent: suggestion.parent,
                    name: name.clone(),
                },
                format!("→ {} (new topic)", path.join(PATH_JOIN)),
                name.as_str().to_owned(),
            )))
        }
    }
}
