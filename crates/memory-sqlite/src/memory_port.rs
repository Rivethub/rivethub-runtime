use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::BrowseFilter;
use kernel::memory::EmbedModelMismatch;
use kernel::memory::Limit;
use kernel::memory::MemoryEntry;
use kernel::memory::MemoryStatsResponse;
use kernel::memory::MemoryStore;
use kernel::memory::MemoryStoreError;
use kernel::memory::PendingTagWire;
use kernel::memory::SearchOutcome;
use kernel::memory::SearchQuery;
use kernel::memory::SummaryDag;
use kernel::memory::SummaryOwner;
use kernel::memory::TagAdd;
use kernel::memory::TagCount;
use kernel::memory::TagCountsQuery;
use kernel::memory::TagDecision;
use kernel::memory::TagId;
use kernel::memory::TagJobContext;
use kernel::memory::TagListQuery;
use kernel::memory::TagLookup;
use kernel::memory::TagSuggestionStored;
use kernel::memory::TagSuggestionWrite;
use kernel::memory::TagTaxonomyDecision;
use kernel::memory::TagTaxonomyEntry;
use kernel::memory::TagTaxonomyMerge;
use kernel::memory::TagTaxonomyMergeResult;
use kernel::memory::TagTaxonomyQuery;
use kernel::memory::TagTaxonomyUpsert;
use kernel::memory::TagWire;
use kernel::owner::Owner;
use kernel::wiki::WikiFilter;
use kernel::wiki::WikiGapsResponse;
use kernel::wiki::WikiLookup;
use kernel::wiki::WikiTopicList;
use kernel::wiki::page::WikiSlug;
use uuid::Uuid;

use crate::SqliteStore;
use crate::error::SqliteError;

#[async_trait]
impl MemoryStore for SqliteStore {
    async fn close(&self) -> Result<(), MemoryStoreError> {
        self.pending.close();
        self.pending.wait().await;
        Ok(())
    }

    async fn probe(&self) -> Result<(), MemoryStoreError> {
        self.with_conn(|conn| {
            conn.query_row("SELECT 1", [], |row| row.get::<_, i32>(0))
                .map(|_| ())
                .map_err(SqliteError::query("probe"))
        })
        .await
        .map_err(Into::into)
    }

    async fn wiki_topics(&self, filter: WikiFilter) -> Result<WikiTopicList, MemoryStoreError> {
        self.read_wiki_topics(filter).await
    }

    async fn wiki_page(&self, slug: &WikiSlug) -> Result<WikiLookup, MemoryStoreError> {
        self.read_wiki_page(slug).await
    }

    async fn wiki_gaps(&self, limit: u32) -> Result<WikiGapsResponse, MemoryStoreError> {
        self.read_wiki_gaps(limit).await
    }

    async fn capture(
        &self,
        batch: kernel::memory::CaptureBatchRequest,
        owner: &Owner,
        node: Option<&str>,
    ) -> Result<kernel::memory::CaptureResult, MemoryStoreError> {
        let now = self.now();
        Self::capture(self, batch, owner, node, now)
            .await
            .map_err(MemoryStoreError::from)
    }

    async fn append(
        &self,
        owner: &Owner,
        batch: Vec<MemoryEntry>,
    ) -> Result<u64, MemoryStoreError> {
        let now = self.now();
        let ids = self
            .append_entries(batch, owner, now)
            .await
            .map_err(MemoryStoreError::from)?;
        u64::try_from(ids.len()).map_err(|error| MemoryStoreError::Query {
            message: error.to_string(),
            source: Box::new(SqliteError::Count(error)),
        })
    }

    async fn search(&self, query: SearchQuery) -> Result<SearchOutcome, MemoryStoreError> {
        let now = self.now();
        self.search_at(query, now, &Utc)
            .await
            .map_err(MemoryStoreError::from)
    }

    async fn browse(&self, filter: BrowseFilter) -> Result<Vec<MemoryEntry>, MemoryStoreError> {
        let now = self.now();
        self.browse_at(filter, now)
            .await
            .map_err(MemoryStoreError::from)
    }

    fn embeddings_armed(&self) -> bool {
        self.embed_armed()
    }

    async fn embed_model_mismatch(&self) -> Result<Option<EmbedModelMismatch>, MemoryStoreError> {
        self.embed_mismatch().await.map_err(MemoryStoreError::from)
    }

    async fn stats(&self) -> Result<MemoryStatsResponse, MemoryStoreError> {
        self.stats_for(None).await.map_err(MemoryStoreError::from)
    }

    async fn capture_activity(
        &self,
    ) -> Result<Vec<kernel::harness_setup::CaptureActivity>, MemoryStoreError> {
        self.capture_activity_read()
            .await
            .map_err(MemoryStoreError::from)
    }

    async fn get_full(&self, id: &str) -> Result<Option<MemoryEntry>, MemoryStoreError> {
        self.message_entry(id).await.map_err(MemoryStoreError::from)
    }

    async fn context(&self, session: &str, now: DateTime<Utc>) -> Result<String, MemoryStoreError> {
        self.context_for(session, now).await
    }

    async fn summary_dag(&self, owner: &SummaryOwner) -> Result<SummaryDag, MemoryStoreError> {
        self.read_summary_dag(owner).await
    }

    async fn list_tags(&self, query: TagListQuery) -> Result<Vec<TagWire>, MemoryStoreError> {
        self.tags_list(query).await
    }

    async fn pending_tags(&self, limit: Limit) -> Result<Vec<PendingTagWire>, MemoryStoreError> {
        self.tags_pending(limit).await
    }

    async fn tag_counts(&self, query: TagCountsQuery) -> Result<Vec<TagCount>, MemoryStoreError> {
        self.tags_counts(query).await
    }

    async fn list_tag_taxonomy(
        &self,
        query: TagTaxonomyQuery,
    ) -> Result<Vec<TagTaxonomyEntry>, MemoryStoreError> {
        self.taxonomy_list(query).await
    }

    async fn decide_tags(
        &self,
        ids: Vec<TagId>,
        decision: TagDecision,
        decided_by: Option<String>,
    ) -> Result<u64, MemoryStoreError> {
        self.tags_decide(ids, decision, decided_by).await
    }

    async fn decide_tag_removals(
        &self,
        ids: Vec<TagId>,
        decision: TagDecision,
        decided_by: Option<String>,
    ) -> Result<u64, MemoryStoreError> {
        self.tags_decide_removals(ids, decision, decided_by).await
    }

    async fn add_tag(&self, add: TagAdd) -> Result<TagWire, MemoryStoreError> {
        self.tags_add(add).await
    }

    async fn lookup_tags(
        &self,
        lookup: TagLookup,
    ) -> Result<BTreeMap<String, Vec<TagWire>>, MemoryStoreError> {
        self.tags_lookup(lookup).await
    }

    async fn upsert_tag_taxonomy(
        &self,
        upsert: TagTaxonomyUpsert,
    ) -> Result<TagTaxonomyEntry, MemoryStoreError> {
        self.taxonomy_upsert(upsert).await
    }

    async fn decide_tag_taxonomy(
        &self,
        decision: TagTaxonomyDecision,
    ) -> Result<u64, MemoryStoreError> {
        self.taxonomy_decide(decision).await
    }

    async fn merge_tag_taxonomy(
        &self,
        merge: TagTaxonomyMerge,
    ) -> Result<TagTaxonomyMergeResult, MemoryStoreError> {
        self.taxonomy_merge(merge).await
    }

    async fn tag_job_context(
        &self,
        summary_id: Uuid,
    ) -> Result<Option<TagJobContext>, MemoryStoreError> {
        self.read_tag_job_context(summary_id).await
    }

    async fn store_tag_suggestions(
        &self,
        write: TagSuggestionWrite,
    ) -> Result<TagSuggestionStored, MemoryStoreError> {
        self.write_tag_suggestions(write).await
    }
}
