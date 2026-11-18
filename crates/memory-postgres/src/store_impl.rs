mod capture_write;

use crate::capture::CaptureOptions;
use crate::error::Error;
use async_trait::async_trait;
use capture_write::Write;
use jiff::Timestamp;
use kernel::memory::BrowseFilter;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::CaptureResult;
use kernel::memory::MemoryEntry;
use kernel::memory::MemoryStore;
use kernel::memory::MemoryStoreError;
use kernel::memory::PendingTagWire;
use kernel::memory::SearchQuery;
use kernel::memory::TagAdd;
use kernel::memory::TagCount;
use kernel::memory::TagCountsQuery;
use kernel::memory::TagDecision;
use kernel::memory::TagId;
use kernel::memory::TagListQuery;
use kernel::memory::TagLookup;
use kernel::memory::TagTaxonomyDecision;
use kernel::memory::TagTaxonomyEntry;
use kernel::memory::TagTaxonomyMerge;
use kernel::memory::TagTaxonomyMergeResult;
use kernel::memory::TagTaxonomyQuery;
use kernel::memory::TagTaxonomyUpsert;
use kernel::memory::TagWire;
use kernel::owner::Owner;
use serde::Deserialize as _;
use std::collections::BTreeMap;
use uuid::Uuid;

const UNKNOWN_CHANNEL: &str = "unknown";

struct EntryGroup {
    session_key: String,
    agent: String,
    channel: Option<String>,
    entries: Vec<MemoryEntry>,
}

fn group_entries(batch: Vec<MemoryEntry>) -> Vec<EntryGroup> {
    let mut groups: Vec<EntryGroup> = Vec::new();
    for entry in batch {
        match groups
            .iter_mut()
            .find(|group| (&group.session_key, &group.agent) == (&entry.session_id, &entry.agent))
        {
            Some(group) => group.entries.push(entry),
            None => groups.push(EntryGroup {
                session_key: entry.session_id.clone(),
                agent: entry.agent.clone(),
                channel: Some(entry.channel.clone()),
                entries: vec![entry],
            }),
        }
    }
    groups
}

fn capture_request(group: &EntryGroup) -> Result<CaptureBatchRequest, MemoryStoreError> {
    let mut messages = Vec::with_capacity(group.entries.len());
    for entry in &group.entries {
        let event_id = entry_event_id(entry)?;
        let mut metadata = entry.metadata.clone();
        metadata
            .get_or_insert_default()
            .insert("event_id", &event_id)
            .map_err(Error::from)?;
        messages.push(CaptureMessage {
            event_id,
            role: entry.role,
            content: entry.content.clone(),
            tool_name: entry.tool_name.clone(),
            tool_args: entry.tool_args.clone(),
            tool_result: entry.tool_result.clone(),
            metadata,
            created_at: entry.created_at,
        });
    }
    Ok(CaptureBatchRequest {
        session_key: group.session_key.clone(),
        agent: group.agent.clone(),
        channel: group.channel.clone(),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages,
    })
}

#[derive(serde::Deserialize)]
struct EntryMetadata {
    event_id: Option<String>,
}

fn entry_event_id(entry: &MemoryEntry) -> Result<String, Error> {
    if let Some(metadata) = &entry.metadata {
        let metadata = EntryMetadata::deserialize(serde::de::value::MapDeserializer::<
            _,
            serde_json::Error,
        >::new(
            metadata
                .as_map()
                .iter()
                .map(|(key, value)| (key.as_str(), value)),
        ))?;
        if let Some(event_id) = metadata.event_id {
            return Ok(event_id);
        }
    }
    append_event_id(entry)
}

fn append_event_id(entry: &MemoryEntry) -> Result<String, Error> {
    use sha2::Digest as _;
    let input = (
        &entry.session_id,
        &entry.agent,
        entry.role,
        &entry.content,
        &entry.tool_name,
    );
    Ok(hex::encode(sha2::Sha256::digest(serde_json::to_vec(
        &input,
    )?)))
}

#[async_trait]
impl MemoryStore for crate::PostgresMemory {
    async fn close(&self) -> Result<(), MemoryStoreError> {
        self.access
            .settled()
            .await
            .map_err(|error| MemoryStoreError::Unavailable(Box::new(error)))
    }

    async fn probe(&self) -> Result<(), MemoryStoreError> {
        self.ping().await.map_err(Into::into)
    }

    fn embeddings_armed(&self) -> bool {
        self.embed.is_some()
    }

    async fn embed_model_mismatch(
        &self,
    ) -> Result<Option<kernel::memory::EmbedModelMismatch>, MemoryStoreError> {
        Ok(None)
    }

    async fn wiki_topics(
        &self,
        filter: kernel::wiki::WikiFilter,
    ) -> Result<kernel::wiki::WikiTopicList, MemoryStoreError> {
        self.read_wiki_topics(filter).await.map_err(Into::into)
    }

    async fn wiki_page(
        &self,
        slug: &kernel::wiki::page::WikiSlug,
    ) -> Result<kernel::wiki::WikiLookup, MemoryStoreError> {
        self.read_wiki_page(slug).await.map_err(Into::into)
    }

    async fn wiki_gaps(
        &self,
        limit: u32,
    ) -> Result<kernel::wiki::WikiGapsResponse, MemoryStoreError> {
        self.read_wiki_gaps(limit).await.map_err(Into::into)
    }

    async fn wiki_imported_authors(
        &self,
        slugs: Vec<String>,
    ) -> Result<BTreeMap<String, Vec<kernel::memory::Provenance>>, MemoryStoreError> {
        Ok(self.wiki_imported_authors(&slugs).await?)
    }

    async fn wiki_archive(
        &self,
        slug: &kernel::wiki::page::WikiSlug,
        month: Option<&str>,
    ) -> Result<Option<String>, MemoryStoreError> {
        self.read_wiki_archive(slug, month)
            .await
            .map_err(Into::into)
    }

    async fn capture(
        &self,
        mut batch: CaptureBatchRequest,
        owner: &Owner,
        node: Option<&str>,
    ) -> Result<CaptureResult, MemoryStoreError> {
        batch
            .channel
            .get_or_insert_with(|| UNKNOWN_CHANNEL.to_owned());
        let options = CaptureOptions {
            owner: owner.clone(),
            node: node.map(str::to_owned),
        };
        self.capture_batch(&batch, &options)
            .await
            .map_err(MemoryStoreError::from)
    }

    async fn mark_summary_settled(
        &self,
        session: &str,
        owner: &Owner,
    ) -> Result<Option<kernel::memory::settlement::SettleResult>, MemoryStoreError> {
        self.mark_settled(session, owner.as_str())
            .await
            .map_err(Into::into)
    }

    async fn append(
        &self,
        owner: &Owner,
        batch: Vec<MemoryEntry>,
    ) -> Result<u64, MemoryStoreError> {
        let mut inserted = 0u64;
        let options = CaptureOptions::owned_by(owner);
        if let Some(entry) = batch
            .iter()
            .find(|entry| kernel::memory::is_imported(&entry.session_id))
        {
            return Err(MemoryStoreError::InvalidRequest {
                message: format!(
                    "{} is an imported session; only a receive writes one",
                    entry.session_id
                ),
            });
        }
        let keys: Vec<&str> = batch
            .iter()
            .map(|entry| entry.session_id.as_str())
            .collect();
        let received: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM rh_conversations
              WHERE owner_user_id = $1 AND session_key = ANY($2) AND settings ? 'receipt')",
        )
        .bind(owner.as_str())
        .bind(&keys)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| Error::operation("append receipt check", source))
        .map_err(MemoryStoreError::from)?;
        if received {
            return Err(Error::ReceivedSession.into());
        }
        for group in group_entries(batch) {
            let request = capture_request(&group)?;
            let result = self
                .capture_keyed(&request, &options, Write::Append)
                .await
                .map_err(MemoryStoreError::from)?;
            inserted += result.inserted;
        }
        Ok(inserted)
    }

    async fn search(
        &self,
        query: SearchQuery,
    ) -> Result<kernel::memory::SearchOutcome, MemoryStoreError> {
        self.complete_search(query).await
    }

    async fn browse(&self, filter: BrowseFilter) -> Result<Vec<MemoryEntry>, MemoryStoreError> {
        self.browse_port(&filter)
            .await
            .map_err(MemoryStoreError::from)
    }

    async fn capture_activity(
        &self,
    ) -> Result<Vec<kernel::harness_setup::CaptureActivity>, MemoryStoreError> {
        self.read_capture_activity().await.map_err(Into::into)
    }

    async fn stats(&self) -> Result<kernel::memory::MemoryStatsResponse, MemoryStoreError> {
        if let Some(answer) = self.stats_cache.fresh() {
            self.probe().await?;
            return Ok(answer);
        }
        let _computing = self.stats_cache.computing().await;
        if let Some(answer) = self.stats_cache.fresh() {
            return Ok(answer);
        }
        let answer = self.memory_stats().await.map_err(MemoryStoreError::from)?;
        self.stats_cache.keep(&answer);
        Ok(answer)
    }

    async fn get_full(&self, id: &str) -> Result<Option<MemoryEntry>, MemoryStoreError> {
        self.memory_get_full(id)
            .await
            .map_err(MemoryStoreError::from)
    }

    async fn summary_dag(
        &self,
        owner: &kernel::memory::SummaryOwner,
    ) -> Result<kernel::memory::SummaryDag, MemoryStoreError> {
        self.read_summary_dag(owner).await
    }

    async fn summary_outline(
        &self,
        owner: &kernel::memory::SummaryOwner,
    ) -> Result<kernel::memory::SummaryDag, MemoryStoreError> {
        self.read_summary_outline(owner).await
    }

    async fn summary_latest(
        &self,
        owner: &kernel::memory::SummaryOwner,
        limit: usize,
    ) -> Result<kernel::memory::SummaryDag, MemoryStoreError> {
        self.read_summary_latest(owner, limit).await
    }

    async fn summary_node(
        &self,
        owner: &kernel::memory::SummaryOwner,
        id: &str,
    ) -> Result<Option<kernel::memory::MemorySummaryNode>, MemoryStoreError> {
        self.read_summary_node(owner, id).await
    }

    async fn summary_tree(
        &self,
        owner: &kernel::memory::SummaryOwner,
        parent: Option<&str>,
        window: kernel::memory::TreeWindow,
    ) -> Result<Option<kernel::memory::SummaryTree>, MemoryStoreError> {
        self.read_summary_tree(owner, parent, window).await
    }

    async fn list_tags(&self, query: TagListQuery) -> Result<Vec<TagWire>, MemoryStoreError> {
        self.tags_list(&query).await
    }

    async fn pending_tags(
        &self,
        limit: kernel::memory::Limit,
    ) -> Result<Vec<PendingTagWire>, MemoryStoreError> {
        self.tags_pending(limit).await
    }

    async fn tag_counts(&self, query: TagCountsQuery) -> Result<Vec<TagCount>, MemoryStoreError> {
        self.tags_counts(&query).await
    }

    async fn list_tag_taxonomy(
        &self,
        query: TagTaxonomyQuery,
    ) -> Result<Vec<TagTaxonomyEntry>, MemoryStoreError> {
        self.tags_taxonomy(&query).await
    }

    async fn decide_tags(
        &self,
        ids: Vec<TagId>,
        decision: TagDecision,
        decided_by: Option<String>,
    ) -> Result<u64, MemoryStoreError> {
        self.tags_decide(&ids, decision, decided_by).await
    }

    async fn add_tag(&self, add: TagAdd) -> Result<TagWire, MemoryStoreError> {
        self.tags_add(&add).await
    }

    async fn lookup_tags(
        &self,
        lookup: TagLookup,
    ) -> Result<BTreeMap<String, Vec<TagWire>>, MemoryStoreError> {
        self.tags_lookup(&lookup).await
    }

    async fn recall_owners(
        &self,
        session_keys: Vec<String>,
    ) -> Result<BTreeMap<String, String>, MemoryStoreError> {
        Ok(self.team_owners(&session_keys).await?)
    }

    async fn recall_identity(
        &self,
    ) -> Result<Option<kernel::memory::RecallIdentity>, MemoryStoreError> {
        Ok(Some(self.read_recall_identity().await?))
    }

    async fn team_sessions(
        &self,
        since: Option<Timestamp>,
        before: Option<Timestamp>,
        limit: u32,
    ) -> Result<Vec<kernel::memory::TeamSession>, MemoryStoreError> {
        self.read_team_sessions(since, before, limit).await
    }

    async fn lookup_origins(
        &self,
        session_keys: Vec<String>,
    ) -> Result<BTreeMap<String, kernel::memory::Provenance>, MemoryStoreError> {
        Ok(self.conversation_origins(&session_keys).await?)
    }

    async fn upsert_tag_taxonomy(
        &self,
        upsert: TagTaxonomyUpsert,
    ) -> Result<TagTaxonomyEntry, MemoryStoreError> {
        self.tags_taxonomy_upsert(&upsert).await
    }

    async fn decide_tag_taxonomy(
        &self,
        decision: TagTaxonomyDecision,
    ) -> Result<u64, MemoryStoreError> {
        self.tags_taxonomy_decide(&decision).await
    }

    async fn merge_tag_taxonomy(
        &self,
        merge: TagTaxonomyMerge,
    ) -> Result<TagTaxonomyMergeResult, MemoryStoreError> {
        self.tags_taxonomy_merge(&merge).await
    }

    async fn tag_job_context(
        &self,
        summary_id: Uuid,
    ) -> Result<Option<kernel::memory::TagJobContext>, MemoryStoreError> {
        self.tags_job_context(summary_id).await
    }

    async fn store_tag_suggestions(
        &self,
        write: kernel::memory::TagSuggestionWrite,
    ) -> Result<kernel::memory::TagSuggestionStored, MemoryStoreError> {
        self.tags_store_suggestions(&write).await
    }
}
