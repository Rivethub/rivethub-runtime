pub mod jobs;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::ops::RangeInclusive;
use std::str::FromStr;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use thiserror::Error;
use uuid::Uuid;
use values::Role;

use crate::BoxError;
use crate::owner::Owner;

pub mod budget;
mod capture;
mod deletion;
mod embed;
mod export;
mod model_title;
mod origin;
mod records;
mod search;
mod suggest;
mod summary;
mod tags;
mod title;
mod tree;

pub use capture::{CaptureBatchRequest, CaptureMessage, CaptureResult};
pub use deletion::DeletedSession;
pub use deletion::EMPTY_SESSIONS_MAX;
pub use deletion::SessionActivity;
pub use deletion::SessionDeletion;
pub use deletion::Speaker;
pub use deletion::SpokenTurn;
pub use deletion::TOMBSTONE_PURGE_BATCH;
pub use deletion::TRASH_MESSAGES_MAX;
pub use deletion::TrashReason;
pub use deletion::TrashSignals;
pub use deletion::TrashVerdict;
pub use deletion::trash_verdict;
pub use deletion::used_since;
pub use embed::EmbedModelMismatch;
pub use embed::EmbedTable;
pub use embed::EmbedTarget;
pub use embed::EmbeddingStore;
pub use embed::EmbeddingStoreError;
pub use embed::LoadedTarget;
pub use embed::StoredChunk;
pub use embed::StoredEmbedding;
pub use embed::Unembeddable;
pub use export::MemoryExport;
pub use model_title::MAX_TITLE_WORDS;
pub use model_title::TitleAnswer;
pub use model_title::fit_title;
pub use model_title::title_schema;
pub use model_title::wants_model_title;
pub use origin::DumpOrigin;
pub use origin::DumpSource;
pub use records::{
    BrowseWindow, BrowseWindowError, MemoryBrowseOrder, MemoryChunkMatch, MemoryDegraded,
    MemoryEntry, MemoryFallback, MemoryHitSource, MemoryMessageTally, MemoryScope,
    MemorySearchResult, MemoryStatsResponse, MemorySummaryNode, RecentSession, SearchMode,
    SummaryKind, TopTool, VectorState,
};
pub use search::SearchOutcome;
pub use search::{SummaryDetail, SummaryExcerpt};
pub use suggest::TagJobContext;
pub use suggest::TagOnEntity;
pub use suggest::TagSuggestKind;
pub use suggest::TagSuggestionRow;
pub use suggest::TagSuggestionStored;
pub use suggest::TagSuggestionWrite;
pub use suggest::TagVocabulary;
pub use suggest::TagVocabularyProposal;
pub use summary::ConversationHead;
pub use summary::ConversationWindow;
pub use summary::HEARTBEAT_SESSION_PREFIX;
pub use summary::NewSummary;
pub use summary::PendingConversation;
pub use summary::SummaryNode;
pub use summary::SummarySource;
pub use summary::SummaryStore;
pub use summary::SummaryStoreError;
pub use summary::SummaryWatch;
pub use summary::SummaryWrite;
pub use summary::ToolUse;
pub use summary::WindowMessage;
pub use summary::WrittenSummary;
pub use tags::{
    PendingTagAction, PendingTagWire, TagCount, TagEntityType, TagEntryPair, TagState,
    TagTaxonomyEntry, TagTaxonomyMerge, TagTaxonomyMergeResult, TagWire,
};
pub use title::SUMMARY_TITLE_MAX_CHARS;
pub use title::names_session;
pub use title::needs_title;
pub use title::spoken_title;
pub use title::summary_title;
pub use title::weak_title;
pub use tree::{SummaryCounts, SummaryShape, SummaryTree, TreeCut, TreeWindow, cut};

#[derive(Debug, Error)]
pub enum MemoryStoreError {
    #[error("memory store is unavailable")]
    Unavailable(#[source] BoxError),
    #[error("memory query failed: {message}")]
    Query {
        message: String,
        #[source]
        source: BoxError,
    },
    #[error("{message}")]
    InvalidRequest { message: String },
    #[error("tag tables are missing: apply migration 0019 (rivetos db migrate)")]
    TagTablesMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TagId(Uuid);

impl TagId {
    #[must_use]
    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn uuid(self) -> Uuid {
        self.0
    }
}

impl FromStr for TagId {
    type Err = uuid::Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Ok(Self(raw.parse()?))
    }
}

impl Display for TagId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TagLiteral {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TagLiteralError {
    #[error("tag must be key:value (got \"{raw}\")")]
    Filter { raw: String },
    #[error("invalid tag literal \"{raw}\" (want key:value)")]
    Literal { raw: String },
}

impl TagLiteral {
    pub fn parse_add(raw: &str) -> Result<Self, TagLiteralError> {
        Self::split(raw, |raw| TagLiteralError::Literal { raw })
    }

    fn split(
        raw: &str,
        error: impl Fn(String) -> TagLiteralError,
    ) -> Result<Self, TagLiteralError> {
        let Some((key, value)) = raw.split_once(':') else {
            return Err(error(raw.to_owned()));
        };
        if key.is_empty() || value.is_empty() {
            return Err(error(raw.to_owned()));
        }
        Ok(Self {
            key: key.to_owned(),
            value: value.to_owned(),
        })
    }
}

impl FromStr for TagLiteral {
    type Err = TagLiteralError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::split(raw, |raw| TagLiteralError::Filter { raw })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    value: u32,
}

impl Limit {
    #[must_use]
    pub fn clamped(raw: Option<u32>, default: u32, range: RangeInclusive<u32>) -> Self {
        Self {
            value: raw.unwrap_or(default).clamp(*range.start(), *range.end()),
        }
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.value
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub mode: SearchMode,
    pub scope: MemoryScope,
    pub agent: Option<String>,
    pub role: Option<Role>,
    pub exclude_roles: Vec<Role>,
    pub tool_name: Option<String>,
    pub tag: Option<TagLiteral>,
    pub limit: Limit,
    pub since: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    pub window: Option<BrowseWindow>,
    pub expand: Option<bool>,
    pub synthesize: Option<bool>,
}

impl SearchQuery {
    #[must_use]
    pub fn resolve_window<Tz: chrono::TimeZone>(mut self, now: DateTime<Utc>, zone: &Tz) -> Self {
        let range = self.time_range(now, zone);
        self.since = range.since;
        self.before = range.before;
        self.window = None;
        self
    }

    #[must_use]
    pub fn time_range<Tz: chrono::TimeZone>(
        &self,
        now: DateTime<Utc>,
        zone: &Tz,
    ) -> crate::memory_core::TimeRange {
        match (self.since, self.before, self.window) {
            (None, None, Some(window)) => crate::memory_core::resolve_window(window, now, zone),
            _ => crate::memory_core::TimeRange {
                since: self.since,
                before: self.before,
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SummaryDag {
    pub session_key: Option<String>,
    pub summaries: Vec<MemorySummaryNode>,
    pub messages: MemoryMessageTally,
}

pub const SUMMARY_PREVIEW_BYTES: usize = 320;

impl SummaryDag {
    #[must_use]
    pub fn outline(mut self) -> Self {
        for node in &mut self.summaries {
            node.source_message_ids.clear();
            if node.content.len() > SUMMARY_PREVIEW_BYTES {
                let mut end = SUMMARY_PREVIEW_BYTES;
                while !node.content.is_char_boundary(end) {
                    end -= 1;
                }
                node.content.truncate(end);
                node.truncated = true;
            }
        }
        self
    }

    #[must_use]
    pub fn latest(mut self, limit: usize) -> Self {
        self.summaries.sort_by(|left, right| {
            right
                .latest_at
                .cmp(&left.latest_at)
                .then_with(|| {
                    (right.kind == SummaryKind::Leaf).cmp(&(left.kind == SummaryKind::Leaf))
                })
                .then_with(|| right.id.cmp(&left.id))
        });
        self.summaries.truncate(limit);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SummaryOwner {
    Session(String),
    Conversation(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseFilter {
    pub conversation_id: Option<Uuid>,
    pub agent: Option<String>,
    pub role: Option<Role>,
    pub tool_name: Option<String>,
    pub tag: Option<TagLiteral>,
    pub window: Option<BrowseWindow>,
    pub since: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    pub include_tools: bool,
    pub order: MemoryBrowseOrder,
    pub limit: Limit,
}

impl BrowseFilter {
    #[must_use]
    pub fn resolve_window<Tz: chrono::TimeZone>(mut self, now: DateTime<Utc>, zone: &Tz) -> Self {
        if self.since.is_none()
            && self.before.is_none()
            && let Some(window) = self.window
        {
            let range = crate::memory_core::resolve_window(window, now, zone);
            self.since = range.since;
            self.before = range.before;
        }
        self.window = None;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagDecision {
    Accept,
    Reject,
}

impl From<TagDecision> for TagState {
    fn from(decision: TagDecision) -> Self {
        match decision {
            TagDecision::Accept => Self::Accepted,
            TagDecision::Reject => Self::Rejected,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TagEntity {
    Id(Uuid),
    SessionKey(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagListQuery {
    pub entity_type: Option<TagEntityType>,
    pub entity_id: Option<Uuid>,
    pub key: Option<String>,
    pub value: Option<String>,
    pub states: Vec<TagState>,
    pub limit: Limit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagCountsQuery {
    pub key: Option<String>,
    pub limit: Limit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagTaxonomyQuery {
    pub key: Option<String>,
    pub state: Option<TagState>,
    pub limit: Limit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagAdd {
    pub entity_type: TagEntityType,
    pub entity: TagEntity,
    pub agent: Option<String>,
    pub tag: TagLiteral,
    pub display: Option<String>,
    pub reason: Option<String>,
    pub decided_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagLookup {
    pub session_keys: Vec<String>,
    pub states: Vec<TagState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagTaxonomyUpsert {
    pub key: String,
    pub value: String,
    pub display: Option<String>,
    pub parent_value: Option<String>,
    pub aliases: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagTaxonomyDecision {
    pub entries: Vec<TagEntryPair>,
    pub decision: TagDecision,
    pub decided_by: Option<String>,
}

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn close(&self) -> Result<(), MemoryStoreError>;

    async fn probe(&self) -> Result<(), MemoryStoreError>;

    fn embeddings_armed(&self) -> bool;

    async fn embed_model_mismatch(&self) -> Result<Option<EmbedModelMismatch>, MemoryStoreError>;

    async fn wiki_topics(
        &self,
        filter: crate::wiki::WikiFilter,
    ) -> Result<crate::wiki::WikiTopicList, MemoryStoreError>;

    async fn wiki_page(
        &self,
        slug: &crate::wiki::page::WikiSlug,
    ) -> Result<crate::wiki::WikiLookup, MemoryStoreError>;

    async fn wiki_gaps(
        &self,
        limit: u32,
    ) -> Result<crate::wiki::WikiGapsResponse, MemoryStoreError>;

    async fn wiki_archive(
        &self,
        slug: &crate::wiki::page::WikiSlug,
        month: Option<&str>,
    ) -> Result<Option<String>, MemoryStoreError> {
        let _: (&crate::wiki::page::WikiSlug, Option<&str>) = (slug, month);
        Ok(None)
    }

    async fn capture(
        &self,
        batch: CaptureBatchRequest,
        owner: &Owner,
        node: Option<&str>,
    ) -> Result<CaptureResult, MemoryStoreError>;

    async fn append(&self, owner: &Owner, batch: Vec<MemoryEntry>)
    -> Result<u64, MemoryStoreError>;

    async fn search(&self, query: SearchQuery) -> Result<SearchOutcome, MemoryStoreError>;

    async fn browse(&self, filter: BrowseFilter) -> Result<Vec<MemoryEntry>, MemoryStoreError>;

    async fn stats(&self) -> Result<MemoryStatsResponse, MemoryStoreError>;

    async fn get_full(&self, id: &str) -> Result<Option<MemoryEntry>, MemoryStoreError>;

    async fn context(&self, session: &str, now: DateTime<Utc>) -> Result<String, MemoryStoreError>;

    async fn summary_dag(&self, owner: &SummaryOwner) -> Result<SummaryDag, MemoryStoreError>;

    async fn summary_outline(&self, owner: &SummaryOwner) -> Result<SummaryDag, MemoryStoreError> {
        Ok(self.summary_dag(owner).await?.outline())
    }

    async fn summary_latest(
        &self,
        owner: &SummaryOwner,
        limit: usize,
    ) -> Result<SummaryDag, MemoryStoreError> {
        Ok(self.summary_dag(owner).await?.latest(limit))
    }

    async fn summary_tree(
        &self,
        owner: &SummaryOwner,
        parent: Option<&str>,
        window: TreeWindow,
    ) -> Result<Option<SummaryTree>, MemoryStoreError> {
        Ok(self.summary_outline(owner).await?.tree(parent, window))
    }

    async fn summary_node(
        &self,
        owner: &SummaryOwner,
        id: &str,
    ) -> Result<Option<MemorySummaryNode>, MemoryStoreError> {
        Ok(self
            .summary_dag(owner)
            .await?
            .summaries
            .into_iter()
            .find(|node| node.id == id))
    }

    async fn list_tags(&self, query: TagListQuery) -> Result<Vec<TagWire>, MemoryStoreError>;

    async fn pending_tags(&self, limit: Limit) -> Result<Vec<PendingTagWire>, MemoryStoreError>;

    async fn tag_counts(&self, query: TagCountsQuery) -> Result<Vec<TagCount>, MemoryStoreError>;

    async fn list_tag_taxonomy(
        &self,
        query: TagTaxonomyQuery,
    ) -> Result<Vec<TagTaxonomyEntry>, MemoryStoreError>;

    async fn decide_tags(
        &self,
        ids: Vec<TagId>,
        decision: TagDecision,
        decided_by: Option<String>,
    ) -> Result<u64, MemoryStoreError>;

    async fn decide_tag_removals(
        &self,
        ids: Vec<TagId>,
        decision: TagDecision,
        decided_by: Option<String>,
    ) -> Result<u64, MemoryStoreError>;

    async fn add_tag(&self, add: TagAdd) -> Result<TagWire, MemoryStoreError>;

    async fn lookup_tags(
        &self,
        lookup: TagLookup,
    ) -> Result<BTreeMap<String, Vec<TagWire>>, MemoryStoreError>;

    async fn upsert_tag_taxonomy(
        &self,
        upsert: TagTaxonomyUpsert,
    ) -> Result<TagTaxonomyEntry, MemoryStoreError>;

    async fn decide_tag_taxonomy(
        &self,
        decision: TagTaxonomyDecision,
    ) -> Result<u64, MemoryStoreError>;

    async fn merge_tag_taxonomy(
        &self,
        merge: TagTaxonomyMerge,
    ) -> Result<TagTaxonomyMergeResult, MemoryStoreError>;

    async fn tag_job_context(
        &self,
        summary_id: Uuid,
    ) -> Result<Option<TagJobContext>, MemoryStoreError>;

    async fn store_tag_suggestions(
        &self,
        write: TagSuggestionWrite,
    ) -> Result<TagSuggestionStored, MemoryStoreError>;
}

pub mod schedule;
