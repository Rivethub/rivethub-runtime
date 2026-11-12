use chrono::DateTime;
use chrono::Utc;
use kernel::memory::BrowseFilter;
use kernel::memory::Limit;
use kernel::memory::MemoryEntry;
use kernel::memory::SearchQuery;
use kernel::memory::TagAdd;
use kernel::memory::TagCountsQuery;
use kernel::memory::TagDecision;
use kernel::memory::TagId;
use kernel::memory::TagListQuery;
use kernel::memory::TagLookup;
use kernel::memory::TagSuggestionWrite;
use kernel::memory::TagTaxonomyDecision;
use kernel::memory::TagTaxonomyMerge;
use kernel::memory::TagTaxonomyQuery;
use kernel::memory::TagTaxonomyUpsert;
use kernel::owner::Owner;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum MemoryRequest {
    Append(Owner, Vec<MemoryEntry>),
    Search(SearchQuery),
    Browse(BrowseFilter),
    Stats,
    GetFull(String),
    Context {
        session: String,
        now: DateTime<Utc>,
    },
    ListTags(TagListQuery),
    PendingTags(Limit),
    TagCounts(TagCountsQuery),
    TagTaxonomy(TagTaxonomyQuery),
    DecideTags {
        ids: Vec<TagId>,
        decision: TagDecision,
        decided_by: Option<String>,
    },
    DecideTagRemovals {
        ids: Vec<TagId>,
        decision: TagDecision,
        decided_by: Option<String>,
    },
    AddTag(TagAdd),
    LookupTags(TagLookup),
    LookupOrigins(Vec<String>),
    UpsertTagTaxonomy(TagTaxonomyUpsert),
    DecideTagTaxonomy(TagTaxonomyDecision),
    MergeTagTaxonomy(TagTaxonomyMerge),
    TagJobContext(Uuid),
    StoreTagSuggestions(TagSuggestionWrite),
    WikiTopics(kernel::wiki::WikiFilter),
    WikiPage(String),
    WikiGaps(u32),
    SummaryDag(kernel::memory::SummaryOwner),
}
