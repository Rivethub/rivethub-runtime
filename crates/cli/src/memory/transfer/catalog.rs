use std::collections::BTreeSet;

use strum::EnumProperty as _;

use super::invalid;
use super::pg::PgColumn;
use crate::Error;

#[derive(
    Clone, Copy, Debug, Eq, PartialEq, strum::Display, strum::EnumIter, strum::EnumProperty,
)]
#[strum(serialize_all = "snake_case")]
pub(super) enum Relation {
    #[strum(props(scope = "member_space"))]
    Spaces,
    #[strum(props(scope = "principal"))]
    SpaceMembers,
    #[strum(props(scope = "principal"))]
    UserSettings,
    AgentPresets,
    Tasks,
    SessionMarks,
    #[strum(props(prefix = "ros"))]
    Conversations,
    #[strum(props(prefix = "ros"))]
    Messages,
    #[strum(props(prefix = "ros"))]
    Summaries,
    #[strum(props(prefix = "ros", scope = "summary_record"))]
    SummaryPassages,
    #[strum(props(prefix = "ros", scope = "summary"))]
    SummarySources,
    #[strum(props(scope = "conversation"))]
    SummaryState,
    #[strum(props(scope = "conversation"))]
    SummaryOpenings,
    #[strum(props(scope = "conversation"))]
    SummaryRuns,
    #[strum(props(scope = "run"))]
    SummaryCheckpoints,
    #[strum(props(prefix = "ros", scope = "conversation"))]
    SummaryDeliveries,
    #[strum(props(scope = "conversation"))]
    SummaryConversionItems,
    Topics,
    #[strum(props(scope = "topic"))]
    TopicAliases,
    #[strum(props(scope = "topic"))]
    TopicRedirects,
    TopicTombstones,
    #[strum(props(scope = "topic"))]
    TopicDecisions,
    SessionPlacements,
    ConversationPlacements,
    SessionSuggestions,
    SessionHistory,
    SessionStartup,
    FilingCorrections,
    WikiScopes,
    #[strum(props(prefix = "ros"))]
    WikiTopics,
    #[strum(props(prefix = "ros"))]
    WikiExtractions,
    #[strum(props(prefix = "ros", scope = "page_and_owner"))]
    WikiCitations,
    #[strum(props(prefix = "ros", scope = "page_and_owner"))]
    WikiProvenance,
    WikiEntityCandidates,
    WikiDeferred,
    #[strum(props(prefix = "ros", scope = "owned_redirect"))]
    WikiRedirects,
    #[strum(props(prefix = "ros"))]
    Tags,
    #[strum(props(prefix = "ros", scope = "taxonomy"))]
    TagTaxonomy,
    EntityExtractions,
    EntityHistory,
    EntityCandidates,
    EntityDecisions,
    EntityEmbeddings,
    EntityApplied,
    EntityTypeSeen,
    #[strum(props(scope = "fact"))]
    EntityFacts,
    #[strum(props(scope = "fact_claim"))]
    EntityFactClaims,
    TidyBatches,
    #[strum(props(scope = "batch"))]
    TidyProposals,
    ReviewMarks,
    SessionTrash,
    SessionTombstones,
    SessionKeeps,
    DailyDigests,
    ModelCalls,
}

#[derive(strum::EnumString)]
#[strum(serialize_all = "snake_case")]
enum Scope {
    Owner,
    Principal,
    MemberSpace,
    Summary,
    SummaryRecord,
    Conversation,
    Run,
    Topic,
    Fact,
    FactClaim,
    OwnedRedirect,
    Taxonomy,
    PageAndOwner,
    Batch,
}

impl Scope {
    fn predicate(self) -> &'static str {
        match self {
            Self::Owner => "r.owner_user_id = $1",
            Self::Principal => "r.user_id = $1",
            Self::MemberSpace => {
                "r.id IN (SELECT space_id FROM rh_space_members WHERE user_id = $1)"
            }
            Self::Summary => {
                "r.summary_id IN (SELECT id FROM rh_summaries WHERE owner_user_id = $1)"
            }
            Self::SummaryRecord => {
                "r.record_id IN (SELECT id FROM rh_summaries WHERE owner_user_id = $1)"
            }
            Self::Conversation => {
                "r.conversation_id IN (SELECT id FROM rh_conversations WHERE owner_user_id = $1)"
            }
            Self::Run => {
                "r.run_id IN (SELECT run.id FROM rh_summary_runs run JOIN rh_conversations c ON c.id = run.conversation_id WHERE c.owner_user_id = $1)"
            }
            Self::Topic => "r.topic_id IN (SELECT id FROM rh_topics WHERE owner_user_id = $1)",
            Self::Fact => {
                "EXISTS (SELECT 1 FROM rh_topics t WHERE t.id = r.topic_id AND (
                    t.owner_user_id = $1 OR (t.owner_user_id IS NULL AND t.kind = 'type'
                    AND EXISTS (SELECT 1 FROM rh_entity_fact_claims c
                                WHERE c.fact_id = r.id AND c.owner_user_id = $1))))"
            }
            Self::FactClaim => {
                "r.owner_user_id = $1 AND EXISTS (
                    SELECT 1 FROM rh_entity_facts f JOIN rh_topics t ON t.id = f.topic_id
                    WHERE f.id = r.fact_id AND (t.owner_user_id = $1
                        OR (t.owner_user_id IS NULL AND t.kind = 'type')))"
            }
            Self::OwnedRedirect => {
                "(r.owner_user_id = $1 OR r.owner_user_id IS NULL) AND r.to_slug IN
                    (SELECT slug FROM rh_wiki_topics WHERE owner_user_id = $1)"
            }
            Self::Taxonomy => {
                "r.owner_user_id = $1 OR (r.owner_user_id IS NULL AND EXISTS (
                    SELECT 1 FROM rh_tags t WHERE t.owner_user_id = $1
                    AND t.key = r.key AND t.value = r.value))"
            }
            Self::PageAndOwner => {
                "r.owner_user_id = $1 AND r.topic_slug IN (SELECT slug FROM rh_wiki_topics WHERE owner_user_id = $1)"
            }
            Self::Batch => {
                "r.batch_id IN (SELECT id FROM rh_tidy_batches WHERE owner_user_id = $1)"
            }
        }
    }
}

impl Relation {
    pub(super) fn sqlite(self) -> String {
        let prefix = self.get_str("prefix").unwrap_or("rh");
        format!("{prefix}_{self}")
    }

    pub(super) fn owner_predicate(self) -> Result<&'static str, Error> {
        let scope = self.get_str("scope").unwrap_or("owner");
        Ok(scope
            .parse::<Scope>()
            .map_err(|error| invalid(format!("invalid transfer scope for {self}: {error}")))?
            .predicate())
    }

    pub(super) fn pg(self) -> String {
        format!("rh_{self}")
    }

    pub(super) fn deletion_counterpart(self) -> Option<Self> {
        if self == Self::Conversations {
            Some(Self::SessionTombstones)
        } else if self == Self::SessionTombstones {
            Some(Self::Conversations)
        } else {
            None
        }
    }

    pub(super) fn authorization(self) -> bool {
        matches!(self, Self::Spaces | Self::SpaceMembers)
    }

    pub(super) fn principal_column(self, name: &str) -> bool {
        matches!(name, "owner_user_id" | "user_id")
            || matches!(
                (self, name),
                (Self::TopicDecisions | Self::Tags, "decided_by")
            )
    }

    pub(super) fn stores_embedding(self) -> bool {
        matches!(self, Self::EntityEmbeddings | Self::SummaryPassages)
    }

    pub(super) fn compares_column(self, name: &str) -> bool {
        !matches!(
            name,
            "embedding" | "embed_status" | "embed_error" | "embed_failures" | "index_needs_repair"
        ) && !matches!(
            (self, name),
            (Self::Topics | Self::Conversations, "visibility")
        )
    }

    pub(super) fn sqlite_extra(self, column: &str) -> bool {
        matches!(
            (self, column),
            (
                Self::Conversations,
                "node" | "last_summary_input_at" | "visibility"
            ) | (Self::WikiTopics, "topic_id" | "archived_sections")
        )
    }
}

#[derive(Debug)]
pub(super) struct Table {
    pub relation: Relation,
    pub columns: Vec<PgColumn>,
    pub local: BTreeSet<String>,
}

impl Table {
    pub(super) fn primary_key(&self) -> Vec<&str> {
        if self.relation == Relation::TopicAliases {
            return vec!["kind", "alias_key"];
        }
        self.columns
            .iter()
            .filter(|column| column.primary)
            .map(|column| column.name.as_str())
            .collect()
    }

    pub(super) fn names(&self) -> Vec<&str> {
        self.columns
            .iter()
            .map(|column| column.name.as_str())
            .collect()
    }
}
