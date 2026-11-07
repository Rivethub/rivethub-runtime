use kernel::memory::Limit;
use kernel::memory::MemoryStoreError;
use kernel::memory::PendingTagAction;
use kernel::memory::PendingTagWire;
use kernel::memory::TagEntityType;
use kernel::memory::TagId;
use kernel::review::ReviewKey;
use kernel::tidy::Proposal;
use kernel::tidy::ProposalState;
use kernel::tidy::TreeEdit;
use kernel::topics::TopicId;
use kernel::topics::TopicName;
use kernel::wiki::page::WikiSlug;
use uuid::Uuid;
use wire::gateway::entities::SessionMention;
use wire::gateway::review::ReviewEvidence;
use wire::gateway::review::ReviewGroup;
use wire::gateway::review::ReviewItem;
use wire::gateway::review::SubjectRef;

use super::Action;
use super::Item;
use super::Topics;
use super::session_of;
use crate::Gateway;
use crate::error::ApiError;

const PENDING_MAX: u32 = 1000;

pub(super) struct TagAction {
    pub(super) id: TagId,
    pub(super) entity_type: TagEntityType,
    pub(super) entity: Uuid,
}

pub(super) async fn tags(gateway: &Gateway) -> Result<Vec<Item>, ApiError> {
    let limit = Limit::clamped(None, PENDING_MAX, 1..=PENDING_MAX);
    let pending = match gateway.store().pending_tags(limit).await {
        Ok(pending) => pending,
        Err(MemoryStoreError::TagTablesMissing) => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    pending
        .iter()
        .map(tag)
        .filter_map(Result::transpose)
        .collect()
}

fn tag(pending: &PendingTagWire) -> Result<Option<Item>, ApiError> {
    let Some(session) = pending.session_key.as_deref().and_then(session_of) else {
        return Ok(None);
    };
    let unreadable = || ApiError::Unavailable("a pending tag has an unreadable id".into());
    let id: TagId = pending.tag.id.parse().map_err(|_| unreadable())?;
    let entity = Uuid::parse_str(&pending.tag.entity_id).map_err(|_| unreadable())?;
    let title = pending
        .title
        .clone()
        .unwrap_or_else(|| session.as_str().to_owned());
    let literal = format!("{}: {}", pending.tag.key, pending.tag.value);
    let verb = match pending.action {
        Some(PendingTagAction::Remove) => "Remove",
        Some(PendingTagAction::Add) | None => "Add",
    };
    let key = ReviewKey::Tag(id);
    let evidence = pending
        .excerpt
        .iter()
        .map(|excerpt| ReviewEvidence {
            session: SessionMention {
                id: session.clone(),
                title: title.clone(),
            },
            at: pending.tag.created_at,
            quote: excerpt.clone(),
            highlight: None,
        })
        .collect();
    Ok(Some(Item {
        body: ReviewItem {
            id: key.to_string(),
            group: ReviewGroup::Tag,
            subject: SubjectRef::Session { id: session, title },
            text: format!("{verb} {literal}"),
            before: None,
            after: Some(literal),
            options: None,
            evidence,
            created_at: pending.tag.created_at,
        },
        key,
        action: Action::Tag(TagAction {
            id,
            entity_type: pending.tag.entity_type,
            entity,
        }),
    }))
}

pub(super) async fn tidy(gateway: &Gateway, topics: &Topics) -> Result<Vec<Item>, ApiError> {
    let batches = gateway
        .tidy()?
        .pending()
        .await
        .map_err(crate::routes::tidy::error)?;
    Ok(batches
        .into_iter()
        .flat_map(|batch| batch.proposals)
        .filter(|proposal| proposal.state == ProposalState::Pending)
        .map(|proposal| proposal_item(&proposal, topics))
        .collect())
}

fn proposal_item(proposal: &Proposal, topics: &Topics) -> Item {
    let (before, after) = diff(&proposal.change, topics);
    let key = ReviewKey::Tidy(proposal.id);
    let evidence = proposal
        .evidence
        .samples
        .iter()
        .filter_map(|sample| {
            let session = session_of(&sample.id)?;
            let title = sample
                .title
                .clone()
                .unwrap_or_else(|| session.as_str().to_owned());
            Some(ReviewEvidence {
                session: SessionMention {
                    id: session,
                    title: title.clone(),
                },
                at: sample.last_active.unwrap_or(proposal.proposed_at),
                quote: title,
                highlight: None,
            })
        })
        .collect();
    Item {
        body: ReviewItem {
            id: key.to_string(),
            group: ReviewGroup::Tidy,
            subject: subject(&proposal.change, topics),
            text: format!(
                "{}. {}",
                text(&proposal.change, topics),
                proposal.reason.as_str()
            ),
            before,
            after,
            options: None,
            evidence,
            created_at: proposal.proposed_at,
        },
        key,
        action: Action::Tidy(proposal.id),
    }
}

fn page(slug: &WikiSlug) -> SubjectRef {
    SubjectRef::Topic {
        id: slug.as_ref().to_owned(),
        title: slug.as_ref().to_owned(),
    }
}

fn subject(change: &TreeEdit, topics: &Topics) -> SubjectRef {
    let topic = |id: TopicId| SubjectRef::Topic {
        id: id.to_string(),
        title: topics.name(id),
    };
    let named = |id: TopicId, name: &TopicName| SubjectRef::Topic {
        id: id.to_string(),
        title: name.as_str().to_owned(),
    };
    match change {
        TreeEdit::Merge { into, .. } => topic(*into),
        TreeEdit::Move { topic: id, .. }
        | TreeEdit::Rename { topic: id, .. }
        | TreeEdit::Close { topic: id }
        | TreeEdit::Delete { topic: id }
        | TreeEdit::FileInto { topic: id, .. } => topic(*id),
        TreeEdit::CreateTopic {
            topic: id, name, ..
        }
        | TreeEdit::AddType {
            topic: id, name, ..
        } => named(*id, name),
        TreeEdit::MergePage { into: slug, .. } | TreeEdit::RemovePage { page: slug } => page(slug),
    }
}

fn text(change: &TreeEdit, topics: &Topics) -> String {
    let quoted = |id: TopicId| format!("“{}”", topics.name(id));
    match change {
        TreeEdit::Merge { source, into } => {
            format!("Merge {} into {}", quoted(*source), quoted(*into))
        }
        TreeEdit::Move {
            topic,
            parent: None,
        } => format!("Move {} to the top level", quoted(*topic)),
        TreeEdit::Move {
            topic,
            parent: Some(parent),
        } => {
            format!("Move {} under {}", quoted(*topic), quoted(*parent))
        }
        TreeEdit::Rename { topic, name } => {
            format!("Rename {} to “{}”", quoted(*topic), name.as_str())
        }
        TreeEdit::Close { topic } => format!("Close {}", quoted(*topic)),
        TreeEdit::Delete { topic } => format!("Delete {}", quoted(*topic)),
        TreeEdit::CreateTopic { name, members, .. } => {
            format!("Create “{}” with {} sessions", name.as_str(), members.len())
        }
        TreeEdit::FileInto { topic, members } => {
            format!("File {} sessions into {}", members.len(), quoted(*topic))
        }
        TreeEdit::MergePage { page, into } => {
            format!("Merge the page {} into {}", page.as_ref(), into.as_ref())
        }
        TreeEdit::RemovePage { page } => format!("Remove the page {}", page.as_ref()),
        TreeEdit::AddType { name, .. } => format!("Add the type “{}”", name.as_str()),
    }
}

fn diff(change: &TreeEdit, topics: &Topics) -> (Option<String>, Option<String>) {
    let name = |id: TopicId| Some(topics.name(id));
    match change {
        TreeEdit::Merge { source, into } => (name(*source), name(*into)),
        TreeEdit::Move { parent, .. } => (None, parent.and_then(name)),
        TreeEdit::Rename { topic, name: to } => (name(*topic), Some(to.as_str().to_owned())),
        TreeEdit::Close { .. } => (None, Some("closed".to_owned())),
        TreeEdit::CreateTopic { name: made, .. } | TreeEdit::AddType { name: made, .. } => {
            (None, Some(made.as_str().to_owned()))
        }
        TreeEdit::MergePage { page, into } => (
            Some(page.as_ref().to_owned()),
            Some(into.as_ref().to_owned()),
        ),
        TreeEdit::Delete { .. } | TreeEdit::FileInto { .. } | TreeEdit::RemovePage { .. } => {
            (None, None)
        }
    }
}
