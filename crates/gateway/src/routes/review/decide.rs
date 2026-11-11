use chrono::DateTime;
use chrono::Utc;
use kernel::facts::FactValue;
use kernel::facts::NewClaim;
use kernel::facts::Said;
use kernel::memory::TagAdd;
use kernel::memory::TagDecision;
use kernel::memory::TagEntity;
use kernel::memory::TagLiteral;
use kernel::review::Mark;
use kernel::review::ReviewKey;
use kernel::review::ReviewMark;
use kernel::review::SNOOZE;
use kernel::tidy::ProposalId;
use kernel::topics::NewTopic;
use kernel::topics::Slug;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKey;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use wire::gateway::review::ReviewDecision;

use super::Action;
use super::Item;
use super::facts::FactAction;
use super::filing::Destination;
use super::filing::FilingAction;
use super::proposals::TagAction;
use crate::Gateway;
use crate::error::ApiError;
use crate::routes::entities::map;
use crate::routes::tidy;
use crate::routes::topics;
use crate::routes::topics::topic_error;
use crate::tenancy::UserId;

enum Verdict {
    Accept,
    Reject,
    Choose(String),
    Edit(String),
}

pub(super) async fn apply(
    gateway: &Gateway,
    user: &UserId,
    item: Item,
    decision: ReviewDecision,
    now: DateTime<Utc>,
) -> Result<ReviewMark, ApiError> {
    let verdict = match decision {
        ReviewDecision::Later => {
            return Ok(ReviewMark {
                key: item.key,
                mark: Mark::Snoozed {
                    until: now + SNOOZE,
                },
            });
        }
        ReviewDecision::Accept => Verdict::Accept,
        ReviewDecision::Reject => Verdict::Reject,
        ReviewDecision::Choose { value } => Verdict::Choose(value),
        ReviewDecision::Edit { value } => Verdict::Edit(value),
    };
    match item.action {
        Action::Fact(fact) => {
            let conflict = matches!(item.key, ReviewKey::Conflict { .. });
            decide_fact(gateway, user, fact, conflict, verdict, now).await?;
        }
        Action::Filing(filing) => decide_filing(gateway, user, filing, verdict, now).await?,
        Action::Tag(tag) => decide_tag(gateway, user, tag, verdict).await?,
        Action::Tidy(proposal) => decide_tidy(gateway, user, proposal, verdict, now).await?,
    }
    Ok(ReviewMark {
        key: item.key,
        mark: Mark::Decided { at: now },
    })
}

fn no_options(group: &str) -> ApiError {
    ApiError::Unprocessable(format!("a {group} has no options to choose from"))
}

async fn decide_fact(
    gateway: &Gateway,
    user: &UserId,
    fact: FactAction,
    conflict: bool,
    verdict: Verdict,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    let value = match verdict {
        Verdict::Accept if conflict => Some(fact.value.clone()),
        Verdict::Accept => None,
        Verdict::Reject if conflict => None,
        Verdict::Reject => fact.previous.clone(),
        Verdict::Choose(value) if conflict => {
            if !fact.options.contains(&value) {
                return Err(ApiError::BadRequest(format!(
                    "{value:?} is not one of the conflict's options"
                )));
            }
            Some(FactValue::new(&value).map_err(map::error)?)
        }
        Verdict::Choose(_) => return Err(no_options("new fact")),
        Verdict::Edit(value) => Some(FactValue::new(&value).map_err(map::error)?),
    };
    let Some(value) = value else {
        return Ok(());
    };
    gateway
        .facts()?
        .assert_facts(&[NewClaim {
            entity: fact.entity,
            key: fact.key,
            value,
            said: Said::You {
                by: user.owner().clone(),
                pinned: fact.pinned,
            },
            owner: Some(user.owner().clone()),
            at: now,
        }])
        .await
        .map_err(map::error)?;
    gateway.sessions().topic_changed(&fact.entity);
    Ok(())
}

async fn decide_filing(
    gateway: &Gateway,
    user: &UserId,
    filing: FilingAction,
    verdict: Verdict,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    let topic = match verdict {
        Verdict::Reject => return Ok(()),
        Verdict::Choose(_) => return Err(no_options("filing suggestion")),
        Verdict::Accept => match filing.destination {
            Destination::Topic(topic) => topic,
            Destination::New { name, parent } => {
                made_topic(gateway, user, name, parent, now).await?
            }
        },
        Verdict::Edit(key) => topics::resolve_id(gateway.topics()?.as_ref(), &key).await?,
    };
    topics::place_session(gateway, user, filing.session, topic).await?;
    Ok(())
}

async fn made_topic(
    gateway: &Gateway,
    user: &UserId,
    name: TopicName,
    parent: Option<TopicId>,
    now: DateTime<Utc>,
) -> Result<TopicId, ApiError> {
    let store = gateway.topics()?;
    let slug = Slug::fold(name.as_str()).map_err(topic_error)?;
    if let Some(found) = store
        .find(&TopicKey::Slug(TopicKind::Work, slug))
        .await
        .map_err(topic_error)?
    {
        return Ok(found.id);
    }
    let topic = Topic::create(
        TopicId::random(),
        NewTopic {
            kind: TopicKind::Work,
            name,
            aliases: Vec::new(),
            parent,
            directory: None,
            owner: Some(user.owner().clone()),
        },
        now,
    )
    .map_err(topic_error)?;
    Ok(store.create(topic).await.map_err(topic_error)?.id)
}

async fn decide_tag(
    gateway: &Gateway,
    user: &UserId,
    tag: TagAction,
    verdict: Verdict,
) -> Result<(), ApiError> {
    let decided_by = Some(user.owner().as_str().to_owned());
    let decision = match verdict {
        Verdict::Accept => TagDecision::Accept,
        Verdict::Reject => TagDecision::Reject,
        Verdict::Choose(_) => return Err(no_options("tag suggestion")),
        Verdict::Edit(raw) => {
            let literal = TagLiteral::parse_add(&raw)
                .map_err(|error| ApiError::BadRequest(error.to_string()))?;
            gateway
                .store()
                .add_tag(TagAdd {
                    entity_type: tag.entity_type,
                    entity: TagEntity::Id(tag.entity),
                    agent: None,
                    tag: literal,
                    display: None,
                    reason: None,
                    decided_by: decided_by.clone(),
                })
                .await?;
            TagDecision::Reject
        }
    };
    gateway
        .store()
        .decide_tags(vec![tag.id], decision, decided_by)
        .await?;
    Ok(())
}

async fn decide_tidy(
    gateway: &Gateway,
    user: &UserId,
    proposal: ProposalId,
    verdict: Verdict,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    match verdict {
        Verdict::Accept => {
            kernel::tidy::approve(&tidy::ports(gateway, user)?, proposal, now)
                .await
                .map_err(tidy::error)?;
        }
        Verdict::Reject => {
            kernel::tidy::reject(gateway.tidy()?.as_ref(), proposal, now)
                .await
                .map_err(tidy::error)?;
        }
        Verdict::Choose(_) | Verdict::Edit(_) => {
            return Err(ApiError::Unprocessable(
                "a tidy proposal is accepted or rejected as it stands".into(),
            ));
        }
    }
    Ok(())
}
