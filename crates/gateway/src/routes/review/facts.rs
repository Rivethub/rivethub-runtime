use std::collections::BTreeSet;

use chrono::DateTime;
use chrono::Utc;
use kernel::facts::FactKey;
use kernel::facts::FactState;
use kernel::facts::FactValue;
use kernel::review::FactReview;
use kernel::review::Quote;
use kernel::review::ReviewKey;
use kernel::review::fact_reviews;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use wire::gateway::review::ReviewEvidence;
use wire::gateway::review::ReviewGroup;
use wire::gateway::review::ReviewItem;
use wire::gateway::review::ReviewOption;
use wire::gateway::review::SubjectRef;
use wire::session_id::SessionId;

use super::Action;
use super::Item;
use super::Topics;
use super::quote_highlight;
use crate::Gateway;
use crate::error::ApiError;
use crate::routes::entities::map;
use crate::routes::entities::map::Titles;

pub(super) struct FactAction {
    pub(super) entity: TopicId,
    pub(super) key: FactKey,
    pub(super) value: FactValue,
    pub(super) previous: Option<FactValue>,
    pub(super) pinned: bool,
    pub(super) options: Vec<String>,
}

pub(super) async fn items(
    gateway: &Gateway,
    topics: &Topics,
    now: DateTime<Utc>,
) -> Result<Vec<Item>, ApiError> {
    let store = gateway.facts()?;
    let entities: BTreeSet<TopicId> = store
        .session_links()
        .await
        .map_err(map::error)?
        .into_iter()
        .map(|link| link.entity)
        .filter(|entity| {
            topics
                .get(*entity)
                .is_some_and(|topic| topic.kind == TopicKind::Entity)
        })
        .collect();
    let mut found = Vec::new();
    for entity in entities {
        let claims = store.claims(entity).await.map_err(map::error)?;
        found.extend(
            fact_reviews(&claims, now)
                .into_iter()
                .map(|review| (entity, review)),
        );
    }
    let sessions: BTreeSet<SessionId> = found
        .iter()
        .flat_map(|(_, review)| &review.quotes)
        .map(|quote| quote.support.session.clone())
        .collect();
    let sessions: Vec<SessionId> = sessions.into_iter().collect();
    let titles = Titles::of(&store.fact_sessions(&sessions).await.map_err(map::error)?);
    Ok(found
        .into_iter()
        .map(|(entity, review)| item(entity, topics, review, &titles))
        .collect())
}

fn item(entity: TopicId, topics: &Topics, review: FactReview, titles: &Titles) -> Item {
    let FactReview {
        key,
        state,
        quotes,
        at,
    } = review;
    let subject = SubjectRef::Entity {
        id: entity.to_string(),
        title: topics.name(entity),
    };
    let evidence = quotes.iter().map(|quote| evidence(quote, titles)).collect();
    let current = state.value.as_str().to_owned();
    let (group, text, before, after, options) = if matches!(key, ReviewKey::Conflict { .. }) {
        let rival = state
            .candidates
            .iter()
            .find(|support| support.value != state.value)
            .map(|support| support.value.as_str().to_owned());
        (
            ReviewGroup::Conflict,
            state.key.as_str().to_owned(),
            Some(current),
            rival,
            Some(choices(&state, titles)),
        )
    } else {
        (
            ReviewGroup::Fact,
            format!("{}: {current}", state.key.as_str()),
            state
                .previous
                .as_ref()
                .map(|previous| previous.value.as_str().to_owned()),
            Some(current),
            None,
        )
    };
    let action = FactAction {
        entity,
        key: state.key.clone(),
        value: state.value.clone(),
        previous: state.previous.map(|previous| previous.value),
        pinned: state.pinned,
        options: options
            .iter()
            .flatten()
            .map(|option| option.value.clone())
            .collect(),
    };
    Item {
        body: ReviewItem {
            id: key.to_string(),
            group,
            subject,
            text,
            before,
            after,
            options,
            evidence,
            created_at: at,
        },
        key,
        action: Action::Fact(action),
    }
}

fn choices(state: &FactState, titles: &Titles) -> Vec<ReviewOption> {
    let current = state.value.as_str();
    let held = state
        .candidates
        .iter()
        .any(|support| support.value == state.value);
    let mut options: Vec<ReviewOption> = Vec::new();
    if !held {
        options.push(ReviewOption {
            label: current.to_owned(),
            value: current.to_owned(),
            source: None,
        });
    }
    options.extend(state.candidates.iter().map(|support| ReviewOption {
        label: support.value.as_str().to_owned(),
        value: support.value.as_str().to_owned(),
        source: Some(map::source(support, titles)),
    }));
    options
}

fn evidence(quote: &Quote, titles: &Titles) -> ReviewEvidence {
    let value = quote.support.value.as_str();
    let words = quote
        .words
        .as_ref()
        .map_or(value, kernel::facts::Evidence::as_str)
        .to_owned();
    ReviewEvidence {
        session: map::mention(&quote.support.session, titles),
        at: quote.support.at,
        highlight: quote_highlight(&words, value),
        quote: words,
    }
}
