#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Relation {
    Conversations,
    Messages,
    Summaries,
    SummarySources,
    SummaryPassages,
    SummaryDeliveries,
    Tags,
    TagTaxonomy,
    WikiTopics,
    WikiProvenance,
    WikiExtractions,
    WikiCitations,
    WikiRedirects,
}

impl Relation {
    pub(super) const ALL: [Self; 13] = [
        Self::Conversations,
        Self::Messages,
        Self::Summaries,
        Self::SummarySources,
        Self::SummaryPassages,
        Self::SummaryDeliveries,
        Self::Tags,
        Self::TagTaxonomy,
        Self::WikiTopics,
        Self::WikiProvenance,
        Self::WikiExtractions,
        Self::WikiCitations,
        Self::WikiRedirects,
    ];

    pub(super) const fn count_sql(self) -> &'static str {
        match self {
            Self::Conversations => "SELECT count(*)::bigint AS count FROM rh_conversations",
            Self::Messages => "SELECT count(*)::bigint AS count FROM rh_messages",
            Self::Summaries => "SELECT count(*)::bigint AS count FROM rh_summaries",
            Self::SummarySources => "SELECT count(*)::bigint AS count FROM rh_summary_sources",
            Self::SummaryPassages => "SELECT count(*)::bigint AS count FROM rh_summary_passages",
            Self::SummaryDeliveries => {
                "SELECT count(*)::bigint AS count FROM rh_summary_deliveries"
            }
            Self::Tags => "SELECT count(*)::bigint AS count FROM rh_tags",
            Self::TagTaxonomy => "SELECT count(*)::bigint AS count FROM rh_tag_taxonomy",
            Self::WikiTopics => "SELECT count(*)::bigint AS count FROM rh_wiki_topics",
            Self::WikiProvenance => "SELECT count(*)::bigint AS count FROM rh_wiki_provenance",
            Self::WikiExtractions => "SELECT count(*)::bigint AS count FROM rh_wiki_extractions",
            Self::WikiCitations => "SELECT count(*)::bigint AS count FROM rh_wiki_citations",
            Self::WikiRedirects => "SELECT count(*)::bigint AS count FROM rh_wiki_redirects",
        }
    }

    pub(super) const fn rh(self) -> &'static str {
        match self {
            Self::Conversations => "rh_conversations",
            Self::Messages => "rh_messages",
            Self::Summaries => "rh_summaries",
            Self::SummarySources => "rh_summary_sources",
            Self::SummaryPassages => "rh_summary_passages",
            Self::SummaryDeliveries => "rh_summary_deliveries",
            Self::Tags => "rh_tags",
            Self::TagTaxonomy => "rh_tag_taxonomy",
            Self::WikiTopics => "rh_wiki_topics",
            Self::WikiProvenance => "rh_wiki_provenance",
            Self::WikiExtractions => "rh_wiki_extractions",
            Self::WikiCitations => "rh_wiki_citations",
            Self::WikiRedirects => "rh_wiki_redirects",
        }
    }

    pub(super) const fn table_name(self) -> &'static str {
        match self {
            Self::Conversations => "conversations",
            Self::Messages => "messages",
            Self::Summaries => "summaries",
            Self::SummarySources => "summary_sources",
            Self::SummaryPassages => "summary_passages",
            Self::SummaryDeliveries => "summary_deliveries",
            Self::Tags => "tags",
            Self::TagTaxonomy => "tag_taxonomy",
            Self::WikiTopics => "wiki_topics",
            Self::WikiProvenance => "wiki_provenance",
            Self::WikiExtractions => "wiki_extractions",
            Self::WikiCitations => "wiki_citations",
            Self::WikiRedirects => "wiki_redirects",
        }
    }

    pub(super) const fn compatibility(self) -> &'static str {
        match self {
            Self::Conversations => "ros_conversations",
            Self::Messages => "ros_messages",
            Self::Summaries => "ros_summaries",
            Self::SummarySources => "ros_summary_sources",
            Self::SummaryPassages => "ros_summary_passages",
            Self::SummaryDeliveries => "ros_summary_deliveries",
            Self::Tags => "ros_tags",
            Self::TagTaxonomy => "ros_tag_taxonomy",
            Self::WikiTopics => "ros_wiki_topics",
            Self::WikiProvenance => "ros_wiki_provenance",
            Self::WikiExtractions => "ros_wiki_extractions",
            Self::WikiCitations => "ros_wiki_citations",
            Self::WikiRedirects => "ros_wiki_redirects",
        }
    }

    pub(super) fn from_compat(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|relation| relation.compatibility() == name)
    }
}
