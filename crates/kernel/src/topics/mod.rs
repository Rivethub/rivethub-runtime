mod categorize;
mod decision;
mod page;
mod page_edit;
mod page_sync;
mod placement;
mod rules;
mod search;
mod startup;
mod tree;
mod values;

use ::values::SessionId;
use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use thiserror::Error;

use crate::BoxError;
use crate::wiki::page::WikiSlug;

pub use categorize::GENERIC_SHARE_PERCENT;
pub use categorize::generic_keys;
pub use categorize::mentioned;
pub use decision::DECISIONS_HEADING;
pub use decision::Decision;
pub use decision::DecisionId;
pub use decision::DecisionStatus;
pub use decision::DecisionStore;
pub use decision::DecisionText;
pub use decision::render_decisions;
pub use decision::topic_decision;
pub use page::PageChange;
pub use page::PageSection;
pub use page::TopicPage;
pub use page::TopicPageError;
pub use page::TopicPageStore;
pub use page::bound_page;
pub(crate) use page::root_page;
pub use page::sync_topic_page;
pub use page_edit::PageEdit;
pub use page_edit::edit_topic_page;
pub use page_sync::SyncTopicPage;
pub use placement::HistoryMark;
pub use placement::Lineage;
pub use placement::PlacedSession;
pub use placement::Placement;
pub use placement::PlacementOutcome;
pub use placement::PlacementSource;
pub use placement::Suggestion;
pub use rules::MergePlan;
pub use rules::check_parent;
pub use search::TopicKey;
pub use search::TopicQuery;
pub use search::by_activity;
pub use search::rank;
pub use startup::DeliveredVia;
pub use startup::STARTUP_BUDGET;
pub use startup::StartupContext;
pub use startup::StartupEntry;
pub use startup::StartupKind;
pub use startup::StartupPart;
pub use startup::StartupParts;
pub use startup::StartupSection;
pub use startup::StartupSource;
pub use startup::StartupStore;
pub use startup::assemble;
pub use startup::render;
pub use startup::startup_context;
pub use tree::MAX_DEPTH;
pub use tree::TopicTree;
pub use tree::page_owner;
pub use tree::root_of;
pub use values::Alias;
pub use values::Change;
pub use values::NewTopic;
pub use values::Redirect;
pub use values::Root;
pub use values::RootKind;
pub use values::Slug;
pub use values::Topic;
pub use values::TopicChanges;
pub use values::TopicId;
pub use values::TopicKind;
pub use values::TopicListing;
pub use values::TopicName;
pub use values::distinct_aliases;
pub(crate) use values::fold;

#[derive(Debug, Error)]
pub enum TopicError {
    #[error("A topic needs a name.")]
    EmptyName,
    #[error("an alias must not be empty")]
    EmptyAlias,
    #[error("\"{text}\" has no letters or digits, so it can't name a topic.")]
    EmptySlug { text: String },
    #[error("There's already a topic called \"{name}\".")]
    SlugTaken { slug: Slug, name: String },
    #[error("\"{alias}\" is already another topic's name.")]
    AliasTaken { alias: String },
    #[error("unknown topic {key}")]
    NotFound { key: String },
    #[error("A topic can't be its own parent.")]
    OwnParent,
    #[error("Work topics nest only under work topics")]
    WorkMisplaced,
    #[error("Types nest only under types")]
    TypeMisplaced,
    #[error("An entity sits only under a type")]
    EntityMisplaced,
    #[error("An entity needs a type")]
    EntityWithoutType,
    #[error("Only topics of the same kind merge")]
    MergeAcrossKinds,
    #[error("\"{slug}\" is not a work topic; sessions are filed only under work topics")]
    NotWork { slug: Slug },
    #[error("Topics nest at most {max} levels deep")]
    TooDeep { max: usize },
    #[error("A topic can't move under its own subtopic")]
    IntoOwnSubtree,
    #[error("A topic can't merge into itself.")]
    SelfMerge,
    #[error("a decision must not be empty")]
    EmptyDecision,
    #[error("unknown decision {key}")]
    UnknownDecision { key: String },
    #[error("decision {id} is superseded")]
    DecisionSuperseded { id: DecisionId },
    #[error("a decision is superseded only by another decision of its topic")]
    ForeignReplacement,
    #[error("\"{slug}\" still has subtopics; move or delete them first.")]
    SubtopicsRemain { slug: Slug },
    #[error("topic store is unavailable")]
    Unavailable(#[source] BoxError),
}

#[async_trait]
pub trait TopicStore: Send + Sync {
    async fn create(&self, topic: Topic) -> Result<Topic, TopicError>;

    async fn update(
        &self,
        id: TopicId,
        changes: TopicChanges,
        now: DateTime<Utc>,
    ) -> Result<Topic, TopicError>;

    async fn merge(
        &self,
        source: TopicId,
        into: TopicId,
        now: DateTime<Utc>,
    ) -> Result<Topic, TopicError>;

    async fn delete(&self, id: TopicId) -> Result<(), TopicError>;

    async fn set_closed(
        &self,
        id: TopicId,
        closed: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<Topic, TopicError>;

    async fn find(&self, key: &TopicKey) -> Result<Option<Topic>, TopicError>;

    async fn list(&self) -> Result<Vec<TopicListing>, TopicError>;

    async fn redirects(&self) -> Result<Vec<Redirect>, TopicError>;

    async fn place(&self, placement: Placement) -> Result<PlacementOutcome, TopicError>;

    async fn unplace(&self, session: &SessionId) -> Result<Option<Placement>, TopicError>;

    async fn placement(&self, session: &SessionId) -> Result<Option<Placement>, TopicError>;

    async fn sessions(&self, topic: TopicId) -> Result<Vec<PlacedSession>, TopicError>;

    async fn mark_history(&self, mark: HistoryMark) -> Result<HistoryMark, TopicError>;

    async fn unmark_history(&self, session: &SessionId) -> Result<(), TopicError>;

    async fn history(&self) -> Result<Vec<HistoryMark>, TopicError>;

    async fn suggest(&self, suggestion: Suggestion) -> Result<(), TopicError>;

    async fn suggestion(&self, session: &SessionId) -> Result<Option<Suggestion>, TopicError>;

    async fn suggestions(&self) -> Result<Vec<Suggestion>, TopicError>;

    async fn page_slug(&self, topic: TopicId) -> Result<Option<WikiSlug>, TopicError>;

    async fn tombstoned(&self, kind: TopicKind, slug: &Slug) -> Result<bool, TopicError>;

    async fn search(
        &self,
        query: &TopicQuery,
        limit: usize,
    ) -> Result<Vec<TopicListing>, TopicError> {
        let listings = self.list().await?;
        let redirects = self.redirects().await?;
        Ok(rank(listings, &redirects, query, limit))
    }
}
