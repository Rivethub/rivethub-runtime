mod suggestions;

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::BrowseFilter;
use kernel::memory::EmbedModelMismatch;
use kernel::memory::Limit;
use kernel::memory::MemoryEntry;
use kernel::memory::MemoryHitSource;
use kernel::memory::MemorySearchResult;
use kernel::memory::MemoryStatsResponse;
use kernel::memory::MemoryStore;
use kernel::memory::MemoryStoreError;
use kernel::memory::PendingTagWire;
use kernel::memory::SearchQuery;
use kernel::memory::SummaryDag;
use kernel::memory::SummaryOwner;
use kernel::memory::TagAdd;
use kernel::memory::TagCount;
use kernel::memory::TagCountsQuery;
use kernel::memory::TagDecision;
use kernel::memory::TagEntity;
use kernel::memory::TagId;
use kernel::memory::TagJobContext;
use kernel::memory::TagListQuery;
use kernel::memory::TagLookup;
use kernel::memory::TagState;
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
use tokio::sync::Mutex;
use uuid::Uuid;

pub use crate::memory_request::MemoryRequest;

#[derive(Debug, Default)]
pub struct FakeMemoryStore {
    entries: Mutex<Vec<MemoryEntry>>,
    search_outcome: Mutex<Option<kernel::memory::SearchOutcome>>,
    requests: Mutex<Vec<MemoryRequest>>,
    wiki_pages: Mutex<BTreeMap<String, kernel::wiki::WikiPageResponse>>,
    wiki_redirects: Mutex<BTreeMap<String, String>>,
    wiki_error: Mutex<Option<MemoryStoreError>>,
    tag_context: Mutex<Option<TagJobContext>>,
    tag_entities: Mutex<BTreeMap<String, Uuid>>,
    embeddings_armed: std::sync::atomic::AtomicBool,
    summary_dags: Mutex<BTreeMap<SummaryOwner, SummaryDag>>,
    embed_mismatch: Mutex<Option<EmbedModelMismatch>>,
}

impl FakeMemoryStore {
    pub fn arm_embeddings(&self) {
        self.embeddings_armed
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub async fn script_embed_mismatch(&self, mismatch: EmbedModelMismatch) {
        *self.embed_mismatch.lock().await = Some(mismatch);
    }

    pub async fn script_session_entity(&self, session: String, entity: Uuid) {
        self.tag_entities.lock().await.insert(session, entity);
    }

    pub async fn fail_next_wiki_request(&self, error: MemoryStoreError) {
        *self.wiki_error.lock().await = Some(error);
    }

    pub async fn set_search_outcome(&self, outcome: kernel::memory::SearchOutcome) {
        *self.search_outcome.lock().await = Some(outcome);
    }

    pub async fn insert_wiki_page(&self, page: kernel::wiki::WikiPageResponse) {
        self.wiki_pages.lock().await.insert(page.slug.clone(), page);
    }

    pub async fn insert_wiki_redirect(&self, from: String, to: String) {
        self.wiki_redirects.lock().await.insert(from, to);
    }

    pub async fn script_summary_dag(&self, owner: SummaryOwner, dag: SummaryDag) {
        self.summary_dags.lock().await.insert(owner, dag);
    }

    pub async fn requests(&self) -> Vec<MemoryRequest> {
        self.requests.lock().await.clone()
    }

    pub async fn script_tag_context(&self, context: TagJobContext) {
        *self.tag_context.lock().await = Some(context);
    }

    async fn record(&self, request: MemoryRequest) {
        self.requests.lock().await.push(request);
    }
}

#[async_trait]
impl MemoryStore for FakeMemoryStore {
    async fn close(&self) -> Result<(), MemoryStoreError> {
        Ok(())
    }

    async fn probe(&self) -> Result<(), MemoryStoreError> {
        Ok(())
    }

    fn embeddings_armed(&self) -> bool {
        self.embeddings_armed
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn embed_model_mismatch(&self) -> Result<Option<EmbedModelMismatch>, MemoryStoreError> {
        Ok(self.embed_mismatch.lock().await.clone())
    }

    async fn wiki_topics(
        &self,
        filter: kernel::wiki::WikiFilter,
    ) -> Result<kernel::wiki::WikiTopicList, MemoryStoreError> {
        self.record(MemoryRequest::WikiTopics(filter.clone())).await;
        let error = self.wiki_error.lock().await.take();
        if let Some(error) = error {
            return Err(error);
        }
        let mut topics: Vec<_> = self
            .wiki_pages
            .lock()
            .await
            .values()
            .filter(|page| {
                filter
                    .tag
                    .as_ref()
                    .is_none_or(|tag| page.tags.contains(tag))
            })
            .filter(|page| {
                filter.query.as_ref().is_none_or(|query| {
                    let query = query.to_lowercase();
                    page.title.to_lowercase().contains(&query)
                        || page.slug.to_lowercase().contains(&query)
                        || page.markdown.to_lowercase().contains(&query)
                })
            })
            .map(wiki_topic)
            .collect();
        topics.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.slug.cmp(&b.slug))
        });
        let total = topics.len() as u64;
        Ok(kernel::wiki::WikiTopicList {
            total,
            topics: topics
                .into_iter()
                .skip(filter.offset as usize)
                .take(filter.limit as usize)
                .collect(),
        })
    }

    async fn wiki_page(
        &self,
        slug: &kernel::wiki::page::WikiSlug,
    ) -> Result<kernel::wiki::WikiLookup, MemoryStoreError> {
        self.record(MemoryRequest::WikiPage(slug.as_ref().to_owned()))
            .await;
        let error = self.wiki_error.lock().await.take();
        if let Some(error) = error {
            return Err(error);
        }
        let page = self.wiki_pages.lock().await.get(slug.as_ref()).cloned();
        if let Some(page) = page {
            return Ok(kernel::wiki::WikiLookup::Page(Box::new(page)));
        }
        Ok(match self.wiki_redirects.lock().await.get(slug.as_ref()) {
            Some(target) => kernel::wiki::WikiLookup::Redirect(target.clone()),
            None => kernel::wiki::WikiLookup::Missing,
        })
    }

    async fn wiki_gaps(
        &self,
        limit: u32,
    ) -> Result<kernel::wiki::WikiGapsResponse, MemoryStoreError> {
        self.record(MemoryRequest::WikiGaps(limit)).await;
        let limit = limit.clamp(1, 200);
        let redirects: Vec<_> = self.wiki_redirects.lock().await.keys().cloned().collect();
        let pages = self.wiki_pages.lock().await;
        let mut missing: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for page in pages.values() {
            for entity in &page.entities {
                let known = pages.values().any(|known| {
                    known.slug.eq_ignore_ascii_case(entity)
                        || known.title.eq_ignore_ascii_case(entity)
                        || known
                            .aliases
                            .iter()
                            .any(|alias| alias.eq_ignore_ascii_case(entity))
                }) || redirects
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(entity));
                if !known {
                    let references = missing.entry(entity.clone()).or_default();
                    if !references.contains(&page.slug) {
                        references.push(page.slug.clone());
                    }
                }
            }
        }
        let mut red_links: Vec<_> = missing
            .into_iter()
            .map(|(entity, referenced_by)| kernel::wiki::WikiRedLink {
                entity,
                referenced_by,
            })
            .collect();
        red_links.sort_by(|a, b| {
            b.referenced_by
                .len()
                .cmp(&a.referenced_by.len())
                .then_with(|| a.entity.cmp(&b.entity))
        });
        red_links.truncate(limit as usize);
        let mut stalest: Vec<_> = pages.values().collect();
        stalest.sort_by_key(|page| {
            (
                page.last_verified
                    .as_ref()
                    .map_or(&page.updated_at, |time| time),
                &page.slug,
            )
        });
        let stalest = stalest
            .into_iter()
            .take(limit as usize)
            .map(wiki_topic)
            .collect();
        drop(pages);
        Ok(kernel::wiki::WikiGapsResponse { red_links, stalest })
    }

    async fn capture(
        &self,
        batch: kernel::memory::CaptureBatchRequest,
        owner: &Owner,
        _node: Option<&str>,
    ) -> Result<kernel::memory::CaptureResult, MemoryStoreError> {
        let mut entries = batch.entries();
        self.record(MemoryRequest::Append(owner.clone(), entries.clone()))
            .await;
        let now = Utc::now();
        let total = entries.len() as u64;
        let inserted = {
            let mut stored = self.entries.lock().await;
            entries.retain(|entry| !stored.iter().any(|seen| seen.id == entry.id));
            for entry in &mut entries {
                entry.created_at.get_or_insert(now);
            }
            let inserted = entries.len() as u64;
            stored.extend(entries);
            inserted
        };
        Ok(kernel::memory::CaptureResult {
            ok: true,
            conversation_id: batch.session_key,
            inserted,
            skipped: total - inserted,
        })
    }

    async fn append(
        &self,
        owner: &Owner,
        mut batch: Vec<MemoryEntry>,
    ) -> Result<u64, MemoryStoreError> {
        let count = batch.len() as u64;
        self.record(MemoryRequest::Append(owner.clone(), batch.clone()))
            .await;
        let now = Utc::now();
        for entry in &mut batch {
            entry.id.get_or_insert_with(|| Uuid::new_v4().to_string());
            entry.created_at.get_or_insert(now);
        }
        self.entries.lock().await.extend(batch);
        Ok(count)
    }

    async fn search(
        &self,
        query: SearchQuery,
    ) -> Result<kernel::memory::SearchOutcome, MemoryStoreError> {
        self.record(MemoryRequest::Search(query.clone())).await;
        let outcome = self.search_outcome.lock().await.clone();
        if let Some(outcome) = outcome {
            return Ok(outcome);
        }
        let needle = query.text.trim().to_owned();
        let entries = self.entries.lock().await;
        let results = entries
            .iter()
            .filter(|e| e.content.contains(&needle))
            .filter(|e| query.agent.as_deref().is_none_or(|a| e.agent == a))
            .take(query.limit.get() as usize)
            .map(|e| {
                Ok(MemorySearchResult {
                    chunk: None,
                    id: e
                        .id
                        .clone()
                        .ok_or_else(|| MemoryStoreError::InvalidRequest {
                            message: "scripted memory entry requires an id".to_owned(),
                        })?,
                    source: MemoryHitSource::Message,
                    content: e.content.clone(),
                    role: Some(e.role),
                    agent: Some(e.agent.clone()),
                    relevance_score: 1.0,
                    created_at: e
                        .created_at
                        .ok_or_else(|| MemoryStoreError::InvalidRequest {
                            message: "scripted memory entry requires a timestamp".to_owned(),
                        })?,
                    session_id: Some(e.session_id.clone()),
                    title: None,
                })
            })
            .collect::<Result<_, MemoryStoreError>>()?;
        drop(entries);
        Ok(kernel::memory::SearchOutcome {
            results,
            ..Default::default()
        })
    }

    async fn browse(&self, filter: BrowseFilter) -> Result<Vec<MemoryEntry>, MemoryStoreError> {
        self.record(MemoryRequest::Browse(filter.clone())).await;
        let entries = self.entries.lock().await;
        Ok(entries
            .iter()
            .filter(|e| filter.agent.as_deref().is_none_or(|a| e.agent == a))
            .filter(|e| filter.role.is_none_or(|r| e.role == r))
            .filter(|e| {
                filter
                    .tool_name
                    .as_deref()
                    .is_none_or(|t| e.tool_name.as_deref() == Some(t))
            })
            .take(filter.limit.get() as usize)
            .cloned()
            .collect())
    }

    async fn stats(&self) -> Result<MemoryStatsResponse, MemoryStoreError> {
        self.record(MemoryRequest::Stats).await;
        let count = self.entries.lock().await.len() as u64;
        Ok(MemoryStatsResponse {
            conversations: 1,
            messages: count,
            tool_calls: 0,
            summaries: 0,
            embed_queue_depth: 0,
            embedded_messages: 0,
            failed_embeddings: 0,
            top_tools: Vec::new(),
            recent_sessions: Vec::new(),
        })
    }

    async fn get_full(&self, id: &str) -> Result<Option<MemoryEntry>, MemoryStoreError> {
        self.record(MemoryRequest::GetFull(id.to_owned())).await;
        Ok(self
            .entries
            .lock()
            .await
            .iter()
            .find(|e| e.id.as_deref() == Some(id))
            .cloned())
    }

    async fn context(&self, session: &str, now: DateTime<Utc>) -> Result<String, MemoryStoreError> {
        self.record(MemoryRequest::Context {
            session: session.to_owned(),
            now,
        })
        .await;
        let entries = self.entries.lock().await;
        Ok(entries
            .iter()
            .filter(|e| e.session_id == session)
            .map(|e| e.content.clone())
            .collect::<Vec<_>>()
            .join("\n"))
    }

    async fn summary_dag(&self, owner: &SummaryOwner) -> Result<SummaryDag, MemoryStoreError> {
        self.record(MemoryRequest::SummaryDag(owner.clone())).await;
        Ok(self
            .summary_dags
            .lock()
            .await
            .get(owner)
            .cloned()
            .unwrap_or_default())
    }

    async fn list_tags(&self, query: TagListQuery) -> Result<Vec<TagWire>, MemoryStoreError> {
        self.record(MemoryRequest::ListTags(query)).await;
        Ok(Vec::new())
    }

    async fn pending_tags(&self, limit: Limit) -> Result<Vec<PendingTagWire>, MemoryStoreError> {
        self.record(MemoryRequest::PendingTags(limit)).await;
        Ok(Vec::new())
    }

    async fn tag_counts(&self, query: TagCountsQuery) -> Result<Vec<TagCount>, MemoryStoreError> {
        self.record(MemoryRequest::TagCounts(query)).await;
        Ok(Vec::new())
    }

    async fn list_tag_taxonomy(
        &self,
        query: TagTaxonomyQuery,
    ) -> Result<Vec<TagTaxonomyEntry>, MemoryStoreError> {
        self.record(MemoryRequest::TagTaxonomy(query)).await;
        Ok(Vec::new())
    }

    async fn decide_tags(
        &self,
        ids: Vec<TagId>,
        decision: TagDecision,
        decided_by: Option<String>,
    ) -> Result<u64, MemoryStoreError> {
        self.record(MemoryRequest::DecideTags {
            ids,
            decision,
            decided_by,
        })
        .await;
        Ok(0)
    }

    async fn decide_tag_removals(
        &self,
        ids: Vec<TagId>,
        decision: TagDecision,
        decided_by: Option<String>,
    ) -> Result<u64, MemoryStoreError> {
        self.record(MemoryRequest::DecideTagRemovals {
            ids,
            decision,
            decided_by,
        })
        .await;
        Ok(0)
    }

    async fn add_tag(&self, add: TagAdd) -> Result<TagWire, MemoryStoreError> {
        self.record(MemoryRequest::AddTag(add.clone())).await;
        let display = match add.display.as_deref().map(str::trim) {
            Some(text) if !text.is_empty() => text.to_owned(),
            _ => add.tag.value.clone(),
        };
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "CONTRACT B6: omitted tag reason is stored as empty text"
            )
        )]
        let reason = add.reason.as_deref().unwrap_or("").to_owned();
        Ok(TagWire {
            id: Uuid::new_v4().to_string(),
            entity_type: add.entity_type,
            entity_id: match &add.entity {
                TagEntity::Id(id) => id.to_string(),
                TagEntity::SessionKey(session) => self
                    .tag_entities
                    .lock()
                    .await
                    .get(session)
                    .ok_or_else(|| MemoryStoreError::InvalidRequest {
                        message: format!("no scripted entity for session {session}"),
                    })?
                    .to_string(),
            },
            key: add.tag.key,
            value: add.tag.value,
            display,
            source: "user".to_owned(),
            state: TagState::Accepted,
            confidence: None,
            proposed_by: String::new(),
            reason,
            decided_by: add.decided_by,
            decided_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
    }

    async fn lookup_tags(
        &self,
        lookup: TagLookup,
    ) -> Result<BTreeMap<String, Vec<TagWire>>, MemoryStoreError> {
        self.record(MemoryRequest::LookupTags(lookup)).await;
        Ok(BTreeMap::new())
    }

    async fn upsert_tag_taxonomy(
        &self,
        upsert: TagTaxonomyUpsert,
    ) -> Result<TagTaxonomyEntry, MemoryStoreError> {
        self.record(MemoryRequest::UpsertTagTaxonomy(upsert.clone()))
            .await;
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "CONTRACT B6: omitted tag display is stored as empty text"
            )
        )]
        let display = upsert.display.as_deref().unwrap_or("").to_owned();
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "CONTRACT B6: omitted tag reason is stored as empty text"
            )
        )]
        let reason = upsert.reason.as_deref().unwrap_or("").to_owned();
        Ok(TagTaxonomyEntry {
            key: upsert.key,
            value: upsert.value,
            display,
            parent_value: upsert.parent_value,
            aliases: upsert.aliases,
            state: TagState::Accepted,
            source: "user".to_owned(),
            reason,
            decided_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
    }

    async fn decide_tag_taxonomy(
        &self,
        decision: TagTaxonomyDecision,
    ) -> Result<u64, MemoryStoreError> {
        self.record(MemoryRequest::DecideTagTaxonomy(decision))
            .await;
        Ok(0)
    }

    async fn merge_tag_taxonomy(
        &self,
        merge: TagTaxonomyMerge,
    ) -> Result<TagTaxonomyMergeResult, MemoryStoreError> {
        self.record(MemoryRequest::MergeTagTaxonomy(merge.clone()))
            .await;
        Ok(TagTaxonomyMergeResult {
            moved: 0,
            dropped: 0,
            into: merge.into,
        })
    }

    async fn tag_job_context(
        &self,
        summary_id: Uuid,
    ) -> Result<Option<TagJobContext>, MemoryStoreError> {
        self.record(MemoryRequest::TagJobContext(summary_id)).await;
        let context = self.tag_context.lock().await.clone();
        Ok(context.filter(|loaded| loaded.summary_id == summary_id))
    }

    async fn store_tag_suggestions(
        &self,
        write: TagSuggestionWrite,
    ) -> Result<TagSuggestionStored, MemoryStoreError> {
        let stored = suggestions::count(&write)?;
        self.record(MemoryRequest::StoreTagSuggestions(write)).await;
        Ok(stored)
    }
}

fn wiki_topic(page: &kernel::wiki::WikiPageResponse) -> kernel::wiki::WikiTopic {
    kernel::wiki::WikiTopic {
        slug: page.slug.clone(),
        title: page.title.clone(),
        tags: page.tags.clone(),
        entities: page.entities.clone(),
        updated_at: page.updated_at,
        excerpt: page.current_state.chars().take(240).collect(),
        topic_id: page.topic_id.clone(),
    }
}
