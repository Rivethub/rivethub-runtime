use chrono::DateTime;
use chrono::Utc;

use super::BatchId;
use super::Proposal;
use super::ProposalId;
use super::ProposalState;
use super::TidyError;
use super::TidyStore;
use super::TreeEdit;
use crate::filing::ConversationPlacement;
use crate::filing::FilingStore;
use crate::filing::Target;
use crate::owner::Owner;
use crate::topics::Change;
use crate::topics::NewTopic;
use crate::topics::Placement;
use crate::topics::PlacementSource;
use crate::topics::Topic;
use crate::topics::TopicChanges;
use crate::topics::TopicId;
use crate::topics::TopicKey;
use crate::topics::TopicKind;
use crate::topics::TopicName;
use crate::topics::TopicPageStore;
use crate::topics::TopicStore;
use crate::wiki::page::FoldMark;

const GONE: &str = "the topic it changes is already gone";
const TARGET_GONE: &str = "the topic it merges into is gone";
const ALREADY_THERE: &str = "it is already there";
const ALREADY_CLOSED: &str = "it is already closed";
const TYPE_EXISTS: &str = "the type already exists";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Reject,
}

#[expect(
    missing_debug_implementations,
    reason = "the tidy, topic and filing ports are trait objects"
)]
pub struct TidyPorts<'a> {
    pub tidy: &'a dyn TidyStore,
    pub topics: &'a dyn TopicStore,
    pub filing: &'a dyn FilingStore,
    pub pages: Option<&'a dyn TopicPageStore>,
    pub owner: &'a Owner,
}

#[derive(Debug)]
pub struct BatchOutcome {
    pub decided: Vec<Proposal>,
    pub skipped: Vec<Skipped>,
    pub failed: Vec<(ProposalId, TidyError)>,
}

#[derive(Debug)]
pub struct Skipped {
    pub proposal: Proposal,
    pub reason: String,
}

enum Approval {
    Applied(Proposal),
    SetAside(Skipped),
}

pub async fn approve(
    ports: &TidyPorts<'_>,
    id: ProposalId,
    now: DateTime<Utc>,
) -> Result<Proposal, TidyError> {
    let proposal = ports
        .tidy
        .proposal(id)
        .await?
        .ok_or(TidyError::UnknownProposal { id })?;
    match approve_pending(ports, &proposal, now).await? {
        Approval::Applied(approved) => Ok(approved),
        Approval::SetAside(skipped) => Err(TidyError::Stale {
            id: skipped.proposal.id,
            reason: skipped.reason,
        }),
    }
}

pub async fn reject(
    tidy: &dyn TidyStore,
    id: ProposalId,
    now: DateTime<Utc>,
) -> Result<Proposal, TidyError> {
    tidy.decide(id, ProposalState::Rejected, now).await
}

pub async fn decide_batch(
    ports: &TidyPorts<'_>,
    id: BatchId,
    decision: Decision,
    now: DateTime<Utc>,
) -> Result<BatchOutcome, TidyError> {
    let batch = ports
        .tidy
        .batch(id)
        .await?
        .ok_or(TidyError::UnknownBatch { id })?;
    let mut outcome = BatchOutcome {
        decided: Vec::new(),
        skipped: Vec::new(),
        failed: Vec::new(),
    };
    for proposal in batch
        .proposals
        .into_iter()
        .filter(|proposal| proposal.state == ProposalState::Pending)
    {
        let decided = match decision {
            Decision::Approve => approve_pending(ports, &proposal, now).await,
            Decision::Reject => reject(ports.tidy, proposal.id, now)
                .await
                .map(Approval::Applied),
        };
        match decided {
            Ok(Approval::Applied(done)) => outcome.decided.push(done),
            Ok(Approval::SetAside(skipped)) => outcome.skipped.push(skipped),
            Err(error) => outcome.failed.push((proposal.id, error)),
        }
    }
    Ok(outcome)
}

async fn approve_pending(
    ports: &TidyPorts<'_>,
    proposal: &Proposal,
    now: DateTime<Utc>,
) -> Result<Approval, TidyError> {
    proposal.pending()?;
    let outdated = match outdated(ports.topics, &proposal.change).await? {
        Some(reason) => Some(reason),
        None => match apply(ports, &proposal.change, now).await {
            Ok(()) => None,
            Err(error) if stale(&error) => Some(error.to_string()),
            Err(error) => return Err(error),
        },
    };
    let Some(reason) = outdated else {
        return ports
            .tidy
            .decide(proposal.id, ProposalState::Approved, now)
            .await
            .map(Approval::Applied);
    };
    let proposal = ports
        .tidy
        .decide(proposal.id, ProposalState::Rejected, now)
        .await?;
    Ok(Approval::SetAside(Skipped { proposal, reason }))
}

async fn outdated(topics: &dyn TopicStore, change: &TreeEdit) -> Result<Option<String>, TidyError> {
    let found = |id: TopicId| async move { topics.find(&TopicKey::Id(id)).await };
    Ok(match change {
        TreeEdit::Merge { source, into } => {
            if found(*source).await?.is_none() {
                Some(GONE.to_owned())
            } else if found(*into).await?.is_none() {
                Some(TARGET_GONE.to_owned())
            } else {
                None
            }
        }
        TreeEdit::Delete { topic } => found(*topic).await?.is_none().then(|| GONE.to_owned()),
        TreeEdit::Rename { topic, name } => match found(*topic).await? {
            None => Some(GONE.to_owned()),
            Some(current) if current.name == *name => Some(format!(
                "it is already named \u{201c}{}\u{201d}",
                name.as_str()
            )),
            Some(_) => None,
        },
        TreeEdit::Move { topic, parent } => match found(*topic).await? {
            None => Some(GONE.to_owned()),
            Some(current) if current.parent == *parent => Some(ALREADY_THERE.to_owned()),
            Some(_) => None,
        },
        TreeEdit::Close { topic } => match found(*topic).await? {
            None => Some(GONE.to_owned()),
            Some(current) if current.closed_at.is_some() => Some(ALREADY_CLOSED.to_owned()),
            Some(_) => None,
        },
        TreeEdit::AddType { topic, .. } => found(*topic)
            .await?
            .is_some()
            .then(|| TYPE_EXISTS.to_owned()),
        TreeEdit::CreateTopic { .. }
        | TreeEdit::FileInto { .. }
        | TreeEdit::MergePage { .. }
        | TreeEdit::RemovePage { .. } => None,
    })
}

const fn stale(error: &TidyError) -> bool {
    matches!(
        error,
        TidyError::Wiki(crate::wiki::builder::WikiStoreError::TopicMissing(_))
            | TidyError::Topic(crate::topics::TopicError::NotFound { .. })
    )
}

async fn apply(
    ports: &TidyPorts<'_>,
    change: &TreeEdit,
    now: DateTime<Utc>,
) -> Result<(), TidyError> {
    let topics = ports.topics;
    match change {
        TreeEdit::Merge { source, into } => {
            topics.merge(*source, *into, now).await?;
        }
        TreeEdit::Move { topic, parent } => {
            let changes = TopicChanges {
                parent: parent.map_or(Change::Clear, Change::Set),
                ..TopicChanges::default()
            };
            topics.update(*topic, changes, now).await?;
        }
        TreeEdit::Rename { topic, name } => {
            let changes = TopicChanges {
                name: Some(name.clone()),
                ..TopicChanges::default()
            };
            topics.update(*topic, changes, now).await?;
        }
        TreeEdit::Close { topic } => {
            topics.set_closed(*topic, Some(now), now).await?;
        }
        TreeEdit::Delete { topic } => topics.delete(*topic).await?,
        TreeEdit::CreateTopic {
            topic,
            name,
            parent,
            aliases,
            members,
        } => {
            let new = NewTopic {
                kind: TopicKind::Work,
                name: name.clone(),
                aliases: aliases.clone(),
                parent: *parent,
                directory: None,
                owner: Some(ports.owner.clone()),
            };
            if !exists(topics, *topic).await? {
                topics.create(Topic::create(*topic, new, now)?).await?;
            }
            for member in members {
                file(ports, member, *topic, now).await?;
            }
        }
        TreeEdit::FileInto { topic, members } => {
            for member in members {
                file(ports, member, *topic, now).await?;
            }
        }
        TreeEdit::MergePage { page, into } => {
            ports
                .pages
                .ok_or(TidyError::PagesUnavailable)?
                .fold_page(page, into, FoldMark::Merged, now)
                .await?;
        }
        TreeEdit::RemovePage { page } => {
            ports
                .pages
                .ok_or(TidyError::PagesUnavailable)?
                .remove_page(page)
                .await?;
        }
        TreeEdit::AddType {
            topic,
            name,
            parent,
        } => create(topics, *topic, TopicKind::Type, name, *parent, now).await?,
    }
    Ok(())
}

async fn exists(topics: &dyn TopicStore, id: TopicId) -> Result<bool, TidyError> {
    Ok(topics.find(&TopicKey::Id(id)).await?.is_some())
}

async fn create(
    topics: &dyn TopicStore,
    id: TopicId,
    kind: TopicKind,
    name: &TopicName,
    parent: Option<TopicId>,
    now: DateTime<Utc>,
) -> Result<(), TidyError> {
    if exists(topics, id).await? {
        return Ok(());
    }
    let new = NewTopic {
        kind,
        name: name.clone(),
        aliases: Vec::new(),
        parent,
        directory: None,
        owner: None,
    };
    topics.create(Topic::create(id, new, now)?).await?;
    Ok(())
}

async fn file(
    ports: &TidyPorts<'_>,
    member: &Target,
    topic: TopicId,
    now: DateTime<Utc>,
) -> Result<(), TidyError> {
    match member {
        Target::Session(session) => {
            ports
                .topics
                .place(Placement {
                    session: session.clone(),
                    topic,
                    by: PlacementSource::Categorizer,
                    at: now,
                    owner: Some(ports.owner.clone()),
                })
                .await?;
        }
        Target::Conversation(conversation) => {
            ports
                .filing
                .place_conversation(ConversationPlacement {
                    conversation: *conversation,
                    topic,
                    by: PlacementSource::Categorizer,
                    at: now,
                })
                .await?;
        }
    }
    Ok(())
}
