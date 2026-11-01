#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Table {
    Conversations,
    Messages,
    Chunks,
    Summaries,
    SummarySources,
    Topics,
    TopicAliases,
    TopicRedirects,
    TopicTombstones,
    TopicDecisions,
    SessionPlacements,
    ConversationPlacements,
    SessionSuggestions,
    SessionHistory,
    SessionStartup,
    SessionTombstones,
    FilingCorrections,
    WikiTopics,
    WikiExtractions,
    WikiCitations,
    WikiProvenance,
    WikiEntityCandidates,
    WikiDeferred,
    WikiRedirects,
    Tags,
    TagTaxonomy,
    Tasks,
    TidyBatches,
    TidyProposals,
    HubState,
}

impl Table {
    pub(super) const ALL: [Self; 30] = [
        Self::Conversations,
        Self::Messages,
        Self::Chunks,
        Self::Summaries,
        Self::SummarySources,
        Self::Topics,
        Self::TopicAliases,
        Self::TopicRedirects,
        Self::TopicTombstones,
        Self::TopicDecisions,
        Self::SessionPlacements,
        Self::ConversationPlacements,
        Self::SessionSuggestions,
        Self::SessionHistory,
        Self::SessionStartup,
        Self::SessionTombstones,
        Self::FilingCorrections,
        Self::WikiTopics,
        Self::WikiExtractions,
        Self::WikiCitations,
        Self::WikiProvenance,
        Self::WikiEntityCandidates,
        Self::WikiDeferred,
        Self::WikiRedirects,
        Self::Tags,
        Self::TagTaxonomy,
        Self::Tasks,
        Self::TidyBatches,
        Self::TidyProposals,
        Self::HubState,
    ];

    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Conversations => "rh_conversations",
            Self::Messages => "rh_messages",
            Self::Chunks => "rh_message_chunks",
            Self::Summaries => "rh_summaries",
            Self::SummarySources => "rh_summary_sources",
            Self::Topics => "rh_topics",
            Self::TopicAliases => "rh_topic_aliases",
            Self::TopicRedirects => "rh_topic_redirects",
            Self::TopicTombstones => "rh_topic_tombstones",
            Self::TopicDecisions => "rh_topic_decisions",
            Self::SessionPlacements => "rh_session_placements",
            Self::ConversationPlacements => "rh_conversation_placements",
            Self::SessionSuggestions => "rh_session_suggestions",
            Self::SessionHistory => "rh_session_history",
            Self::SessionStartup => "rh_session_startup",
            Self::SessionTombstones => "rh_session_tombstones",
            Self::FilingCorrections => "rh_filing_corrections",
            Self::WikiTopics => "rh_wiki_topics",
            Self::WikiExtractions => "rh_wiki_extractions",
            Self::WikiCitations => "rh_wiki_citations",
            Self::WikiProvenance => "rh_wiki_provenance",
            Self::WikiEntityCandidates => "rh_wiki_entity_candidates",
            Self::WikiDeferred => "rh_wiki_deferred",
            Self::WikiRedirects => "rh_wiki_redirects",
            Self::Tags => "rh_tags",
            Self::TagTaxonomy => "rh_tag_taxonomy",
            Self::Tasks => "rh_tasks",
            Self::TidyBatches => "rh_tidy_batches",
            Self::TidyProposals => "rh_tidy_proposals",
            Self::HubState => "rh_hub_state",
        }
    }

    pub(super) const fn owned(self) -> bool {
        matches!(
            self,
            Self::Conversations
                | Self::Messages
                | Self::Chunks
                | Self::Summaries
                | Self::SessionPlacements
                | Self::ConversationPlacements
                | Self::WikiTopics
                | Self::WikiExtractions
                | Self::WikiCitations
                | Self::WikiProvenance
                | Self::WikiEntityCandidates
                | Self::WikiDeferred
                | Self::Tasks
                | Self::TidyBatches
                | Self::SessionHistory
        )
    }
}
