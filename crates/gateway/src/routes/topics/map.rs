use kernel::filing::FiledConversation;
use kernel::filing::FiledSession;
use kernel::topics::Alias;
use kernel::topics::Decision;
use kernel::topics::DecisionStatus;
use kernel::topics::HistoryMark;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::Root;
use kernel::topics::Suggestion;
use kernel::topics::Topic;
use kernel::topics::TopicError;
use kernel::topics::TopicKind;
use kernel::topics::TopicListing;
use kernel::topics::TopicPageError;
use wire::gateway::topics::ConversationPlacement;
use wire::gateway::topics::PlacedBy;
use wire::gateway::topics::SessionHistoryMark;
use wire::gateway::topics::TopicConversation;
use wire::gateway::topics::TopicDecision;
use wire::gateway::topics::TopicDirectory;
use wire::gateway::topics::TopicKind as TopicKindBody;
use wire::gateway::topics::TopicPlacement;
use wire::gateway::topics::TopicRecord;
use wire::gateway::topics::TopicSession;
use wire::gateway::topics::TopicSuggestion;
use wire::gateway::topics::TopicSummary;

use crate::error::ApiError;

pub(crate) fn error(error: TopicError) -> ApiError {
    match error {
        TopicError::EmptyName
        | TopicError::EmptyAlias
        | TopicError::EmptySlug { .. }
        | TopicError::OwnParent
        | TopicError::EntityWithoutType
        | TopicError::SelfMerge
        | TopicError::EmptyDecision
        | TopicError::ForeignReplacement => ApiError::BadRequest(error.to_string()),
        TopicError::SlugTaken { .. }
        | TopicError::AliasTaken { .. }
        | TopicError::TooDeep { .. }
        | TopicError::IntoOwnSubtree
        | TopicError::WorkMisplaced
        | TopicError::TypeMisplaced
        | TopicError::EntityMisplaced
        | TopicError::MergeAcrossKinds
        | TopicError::NotWork { .. }
        | TopicError::SubtopicsRemain { .. }
        | TopicError::DecisionSuperseded { .. } => ApiError::Conflict(error.to_string()),
        TopicError::NotFound { .. } | TopicError::UnknownDecision { .. } => {
            ApiError::Missing(error.to_string())
        }
        TopicError::Unavailable(source) => {
            tracing::error!("topic store failed: {source}");
            ApiError::Unavailable("topic store unavailable".into())
        }
    }
}

pub(crate) fn page_error(error: TopicPageError) -> ApiError {
    match error {
        TopicPageError::Topic(error) => self::error(error),
        TopicPageError::Memory(error) => ApiError::Store(error),
        TopicPageError::Wiki(_)
        | TopicPageError::Payload(_)
        | TopicPageError::ParentPlacement(_) => {
            tracing::error!("topic page lookup failed: {error}");
            ApiError::Internal
        }
    }
}

pub(super) fn aliases(texts: &[String]) -> Result<Vec<Alias>, ApiError> {
    texts
        .iter()
        .map(|text| Alias::new(text).map_err(error))
        .collect()
}

pub(super) const fn kind(kind: TopicKindBody) -> TopicKind {
    match kind {
        TopicKindBody::Work => TopicKind::Work,
        TopicKindBody::Type => TopicKind::Type,
        TopicKindBody::Entity => TopicKind::Entity,
    }
}

const fn kind_body(kind: TopicKind) -> TopicKindBody {
    match kind {
        TopicKind::Work => TopicKindBody::Work,
        TopicKind::Type => TopicKindBody::Type,
        TopicKind::Entity => TopicKindBody::Entity,
    }
}

pub(super) fn root(directory: TopicDirectory) -> Root {
    match directory {
        TopicDirectory::Local { node, path } => Root::Local { node, path },
        TopicDirectory::Shared { path } => Root::Shared { path },
    }
}

fn directory(root: Root) -> TopicDirectory {
    match root {
        Root::Local { node, path } => TopicDirectory::Local { node, path },
        Root::Shared { path } => TopicDirectory::Shared { path },
    }
}

const fn placed_by(source: PlacementSource) -> PlacedBy {
    match source {
        PlacementSource::You => PlacedBy::You,
        PlacementSource::Categorizer => PlacedBy::Categorizer,
    }
}

pub(super) fn record(topic: Topic) -> TopicRecord {
    TopicRecord {
        id: topic.id.to_string(),
        kind: kind_body(topic.kind),
        slug: topic.slug.to_string(),
        name: topic.name.as_str().to_owned(),
        aliases: topic
            .aliases
            .iter()
            .map(|alias| alias.as_str().to_owned())
            .collect(),
        parent: topic.parent.map(|parent| parent.to_string()),
        directory: topic.directory.map(directory),
        created_at: topic.created_at,
        updated_at: topic.updated_at,
        closed_at: topic.closed_at,
        owner: topic.owner.map(|owner| owner.as_str().to_owned()),
        visibility: Some(topic.visibility.as_str().to_owned()),
        visibility_mark: topic.visibility_mark.map(|mark| mark.as_str().to_owned()),
    }
}

pub(super) fn summary(listing: TopicListing) -> TopicSummary {
    TopicSummary {
        topic: record(listing.topic),
        session_count: listing.session_count,
        last_active: listing.last_active,
    }
}

pub(super) fn placement(placement: Placement) -> TopicPlacement {
    TopicPlacement {
        session_id: placement.session,
        topic_id: placement.topic.to_string(),
        by: placed_by(placement.by),
        at: placement.at,
    }
}

pub(super) fn history_mark(mark: HistoryMark) -> SessionHistoryMark {
    SessionHistoryMark {
        session_id: mark.session,
        marked_at: mark.at,
    }
}

pub(super) fn suggestion(suggestion: &Suggestion) -> TopicSuggestion {
    TopicSuggestion {
        name: suggestion.name.as_str().to_owned(),
        topic_id: suggestion.topic.map(|topic| topic.to_string()),
        parent_id: suggestion.parent.map(|topic| topic.to_string()),
    }
}

pub(super) fn filed_session(filed: FiledSession) -> TopicSession {
    TopicSession {
        placement: placement(filed.placement),
        title: filed.title,
        last_active: filed.last_active,
        node: filed.node,
        parent_session_id: filed.lineage.as_ref().map(|lineage| lineage.parent.clone()),
        task_id: filed.lineage.map(|lineage| lineage.task.to_string()),
        visibility: filed
            .visibility
            .as_ref()
            .map(|state| state.effective.as_str().to_owned()),
        visibility_mark: filed
            .visibility
            .and_then(|state| state.mark)
            .map(|mark| mark.as_str().to_owned()),
    }
}

pub(super) fn conversation_placement(
    placement: &kernel::filing::ConversationPlacement,
) -> ConversationPlacement {
    ConversationPlacement {
        conversation_id: placement.conversation.to_string(),
        topic_id: placement.topic.to_string(),
        by: placed_by(placement.by),
        at: placement.at,
    }
}

pub(super) fn filed_conversation(filed: FiledConversation) -> TopicConversation {
    TopicConversation {
        placement: conversation_placement(&filed.placement),
        channel: filed.channel,
        title: filed.title,
        last_active: filed.last_active,
        node: filed.node,
    }
}

pub(super) fn decision(decision: Decision) -> TopicDecision {
    TopicDecision {
        id: decision.id.to_string(),
        topic_id: decision.topic.to_string(),
        text: decision.text.as_str().to_owned(),
        status: match decision.status {
            DecisionStatus::Active => wire::gateway::topics::DecisionStatus::Active,
            DecisionStatus::Superseded => wire::gateway::topics::DecisionStatus::Superseded,
        },
        superseded_by: decision.superseded_by.map(|id| id.to_string()),
        by: decision.by,
        session_id: decision.session,
        created_at: decision.created_at,
        updated_at: decision.updated_at,
    }
}
