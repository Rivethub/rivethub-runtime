use crate::embed::EmbedAttempt;
use crate::error::Error;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryChunkMatch;
use kernel::memory::MemoryFallback;
use kernel::memory::MemoryHitSource;
use kernel::memory::MemoryScope;
use kernel::memory::SearchMode;
use kernel::memory::SummaryKind;
use kernel::memory::TagLiteral;
use kernel::memory::TagWire;
use kernel::memory_core::FusionItem;
use kernel::memory_core::HYBRID_RRF_K;
use kernel::memory_core::fusion_score;
use kernel::memory_core::literal_scan;
use kernel::memory_core::passes_fusion_gate;
use kernel::memory_core::reciprocal_rank_fusion;
use kernel::memory_core::should_trigram_fallback;
use uuid::Uuid;
use wire::message::Role;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchOptions {
    pub(crate) since: Option<DateTime<Utc>>,
    pub(crate) before: Option<DateTime<Utc>>,
    pub(crate) text: String,
    pub(crate) mode: SearchMode,
    pub(crate) scope: MemoryScope,
    pub(crate) limit: u32,
    pub(crate) agent: Option<String>,
    pub(crate) role: Option<Role>,
    pub(crate) exclude_roles: Vec<Role>,
    pub(crate) tool_name: Option<String>,
    pub(crate) tag: Option<TagLiteral>,
}

impl SearchOptions {
    fn without_nul(&self) -> Self {
        let strip = |text: &str| text.replace('\0', "");
        Self {
            text: strip(&self.text),
            agent: self.agent.as_deref().map(strip),
            tool_name: self.tool_name.as_deref().map(strip),
            tag: self.tag.as_ref().map(|tag| TagLiteral {
                key: strip(&tag.key),
                value: strip(&tag.value),
            }),
            exclude_roles: self.exclude_roles.clone(),
            ..*self
        }
    }

    pub(crate) fn admits(&self, role: Role) -> bool {
        self.role.is_none_or(|wanted| wanted == role) && !self.exclude_roles.contains(&role)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemorySearchHit {
    pub chunk: Option<MemoryChunkMatch>,
    pub id: String,
    pub source: MemoryHitSource,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub score: f64,
    pub role: Option<Role>,
    pub agent: Option<String>,
    pub kind: Option<SummaryKind>,
    pub tool_name: Option<String>,
    pub conversation_id: Option<String>,
    pub session_id: Option<String>,
    pub fallback: Option<MemoryFallback>,
    pub tags: Option<Vec<TagWire>>,
    pub title: Option<String>,
    pub(crate) content_key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchOutcome {
    pub hits: Vec<MemorySearchHit>,
    pub degraded: Option<String>,
    pub fallback: Option<MemoryFallback>,
}

#[derive(Debug, Clone)]
pub(crate) struct ArmRow {
    pub(crate) chunk: Option<kernel::memory::MemoryChunkMatch>,
    pub(crate) id: String,
    pub(crate) source: MemoryHitSource,
    pub(crate) content_key: String,
    pub(crate) reasoning: bool,
    pub(crate) role: Option<Role>,
    pub(crate) agent: Option<String>,
    pub(crate) kind: Option<kernel::memory::SummaryKind>,
    pub(crate) tool_name: Option<String>,
    pub(crate) conversation_id: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) arm_score: f64,
    pub(crate) semantic: f64,
    pub(crate) boost: f64,
}

impl FusionItem for ArmRow {
    fn fusion_id(&self) -> &str {
        &self.id
    }

    fn fusion_source(&self) -> MemoryHitSource {
        self.source
    }

    fn fusion_boost(&self) -> f64 {
        self.boost
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArmKind {
    Fts,
    FtsExplicit,
    Trigram,
    Regex,
}

struct FusedEntry {
    hit: MemorySearchHit,
    arm_count: u32,
}

fn fuse(fts: &[ArmRow], vector: Option<&[ArmRow]>, trigram: Option<&[ArmRow]>) -> Vec<FusedEntry> {
    let mut arms: Vec<&[ArmRow]> = Vec::with_capacity(3);
    arms.push(fts);
    if let Some(rows) = vector {
        arms.push(rows);
    }
    if let Some(rows) = trigram {
        arms.push(rows);
    }
    let mut fused: Vec<FusedEntry> = reciprocal_rank_fusion(&arms, HYBRID_RRF_K)
        .into_iter()
        .map(|ranked| {
            let row = ranked.item;
            FusedEntry {
                arm_count: ranked.arm_count,
                hit: MemorySearchHit {
                    chunk: row.chunk.clone(),
                    id: row.id.clone(),
                    source: row.source,
                    content: String::new(),
                    role: row.role,
                    agent: row.agent.clone(),
                    kind: row.kind,
                    tool_name: row.tool_name.clone(),
                    conversation_id: row.conversation_id.clone(),
                    created_at: row.created_at,
                    score: fusion_score(ranked.rrf, row),
                    fallback: None,
                    session_id: row.session_id.clone(),
                    tags: None,
                    title: None,
                    content_key: row.content_key.clone(),
                },
            }
        })
        .collect();
    fused.sort_by(|left, right| {
        right
            .hit
            .score
            .total_cmp(&left.hit.score)
            .then_with(|| left.hit.id.cmp(&right.hit.id))
    });
    fused
}

pub(crate) struct TagFilter {
    pub(crate) key: String,
    pub(crate) value: String,
}

impl From<&TagLiteral> for TagFilter {
    fn from(literal: &TagLiteral) -> Self {
        Self {
            key: literal.key.clone(),
            value: literal.value.clone(),
        }
    }
}

fn finish_rows(rows: Vec<ArmRow>, limit: u32) -> Result<Vec<MemorySearchHit>, Error> {
    let hits: Vec<MemorySearchHit> = rows
        .into_iter()
        .map(|row| {
            let score =
                kernel::memory_core::composite_score(row.arm_score, row.semantic, row.boost);
            MemorySearchHit {
                chunk: row.chunk,
                id: row.id,
                source: row.source,
                content: String::new(),
                role: row.role,
                agent: row.agent,
                kind: row.kind,
                tool_name: row.tool_name,
                conversation_id: row.conversation_id,
                created_at: row.created_at,
                score,
                fallback: None,
                session_id: row.session_id,
                tags: None,
                title: None,
                content_key: row.content_key,
            }
        })
        .collect();
    let mut hits = distinct(hits);
    hits.truncate(usize::try_from(limit.max(1))?);
    Ok(hits)
}

fn said_first(mut rows: Vec<ArmRow>) -> Vec<ArmRow> {
    rows.sort_by_key(|row| row.reasoning);
    rows
}

fn distinct(hits: Vec<MemorySearchHit>) -> Vec<MemorySearchHit> {
    let mut seen = std::collections::BTreeSet::new();
    hits.into_iter()
        .filter(|hit| {
            seen.insert((
                hit.source == MemoryHitSource::Summary,
                hit.session_id.clone(),
                hit.created_at,
                hit.content_key.clone(),
            ))
        })
        .collect()
}

impl crate::PostgresMemory {
    pub(crate) async fn search_full(
        &self,
        options: &SearchOptions,
    ) -> Result<Vec<MemorySearchHit>, Error> {
        Ok(self.search_outcome(options).await?.hits)
    }

    pub async fn search_outcome_for(
        &self,
        query: &kernel::memory::SearchQuery,
    ) -> Result<SearchOutcome, Error> {
        if query.window.is_some() && query.since.is_none() && query.before.is_none() {
            return Err(Error::UnresolvedWindow);
        }
        self.search_outcome(&SearchOptions {
            since: query.since,
            before: query.before,
            text: query.text.clone(),
            mode: query.mode,
            scope: query.scope,
            limit: query.limit.get(),
            agent: query.agent.clone(),
            role: query.role,
            exclude_roles: query.exclude_roles.clone(),
            tool_name: query.tool_name.clone(),
            tag: query.tag.clone(),
        })
        .await
    }

    pub(crate) async fn search_outcome(
        &self,
        options: &SearchOptions,
    ) -> Result<SearchOutcome, Error> {
        let options = &options.without_nul();
        let tag_filter = options.tag.as_ref().map(TagFilter::from);
        let mut outcome = match options.mode {
            SearchMode::Hybrid => self.hybrid_search(options, tag_filter.as_ref()).await,
            SearchMode::Vector => self.vector_search(options, tag_filter.as_ref()).await,
            SearchMode::Fts | SearchMode::Trigram | SearchMode::Regex => {
                self.explicit_search(options, tag_filter.as_ref(), options.mode)
                    .await
            }
        }?;
        self.fill_contents(&mut outcome.hits).await?;
        Ok(outcome)
    }

    async fn vector_search(
        &self,
        options: &SearchOptions,
        tag_filter: Option<&TagFilter>,
    ) -> Result<SearchOutcome, Error> {
        match self.embed_query(&options.text).await {
            EmbedAttempt::NotWired => {
                self.explicit_search(options, tag_filter, SearchMode::Fts)
                    .await
            }
            EmbedAttempt::Embedded { vector, ef_search } => {
                let rows = self
                    .vector_arm_rows(options, tag_filter, &vector, ef_search)
                    .await?;
                let hits = finish_rows(rows, options.limit)?;
                self.record_access(&hits)?;
                Ok(SearchOutcome {
                    hits,
                    degraded: None,
                    fallback: None,
                })
            }
            EmbedAttempt::Failed {
                failure,
                elapsed_ms,
            } => {
                self.vector_dropped(&failure, elapsed_ms);
                let reason = failure.reason();
                self.fts_explicit(options, tag_filter, None, Some(reason))
                    .await
            }
        }
    }

    async fn explicit_search(
        &self,
        options: &SearchOptions,
        tag_filter: Option<&TagFilter>,
        mode: SearchMode,
    ) -> Result<SearchOutcome, Error> {
        if mode != SearchMode::Fts {
            let kind = match mode {
                SearchMode::Regex => ArmKind::Regex,
                SearchMode::Hybrid | SearchMode::Fts | SearchMode::Vector | SearchMode::Trigram => {
                    ArmKind::Trigram
                }
            };
            let rows = self.arm_rows(options, kind, tag_filter, None).await?;
            let hits = finish_rows(rows, options.limit)?;
            self.record_access(&hits)?;
            return Ok(SearchOutcome {
                hits,
                degraded: None,
                fallback: None,
            });
        }
        let mut degraded = None;
        let query = match self.embed_query(&options.text).await {
            EmbedAttempt::NotWired => None,
            EmbedAttempt::Embedded { vector, .. } => Some(vector),
            EmbedAttempt::Failed {
                failure,
                elapsed_ms,
            } => {
                self.vector_warn_only(&failure, elapsed_ms);
                degraded = Some(failure.reason());
                None
            }
        };
        self.fts_explicit(options, tag_filter, query, degraded)
            .await
    }

    async fn fts_explicit(
        &self,
        options: &SearchOptions,
        tag_filter: Option<&TagFilter>,
        query: Option<Vec<f32>>,
        degraded: Option<String>,
    ) -> Result<SearchOutcome, Error> {
        let rows = self
            .arm_rows(options, ArmKind::FtsExplicit, tag_filter, query.as_deref())
            .await?;
        let hits = if rows.is_empty() && should_trigram_fallback(&options.text) {
            let recovered = said_first(
                self.arm_rows(options, ArmKind::Trigram, tag_filter, query.as_deref())
                    .await?,
            );
            let recovered_nonempty = !recovered.is_empty();
            let mut converted = finish_rows(recovered, options.limit)?;
            if recovered_nonempty {
                for hit in &mut converted {
                    hit.fallback = Some(MemoryFallback::Trigram);
                }
            }
            converted
        } else {
            finish_rows(said_first(rows), options.limit)?
        };
        self.record_access(&hits)?;
        Ok(SearchOutcome {
            hits,
            degraded,
            fallback: None,
        })
    }

    async fn hybrid_search(
        &self,
        options: &SearchOptions,
        tag_filter: Option<&TagFilter>,
    ) -> Result<SearchOutcome, Error> {
        let embed = self.embed_query(&options.text).await;
        let query = embed.vector().map(Vec::as_slice);
        let literal = literal_scan(&options.text);
        let trigram = async {
            if literal {
                self.arm_rows(options, ArmKind::Trigram, tag_filter, query)
                    .await
                    .map(Some)
            } else {
                Ok(None)
            }
        };
        let vector = async {
            match &embed {
                EmbedAttempt::Embedded { vector, ef_search } => self
                    .vector_arm_rows(options, tag_filter, vector, *ef_search)
                    .await
                    .map(Some),
                EmbedAttempt::NotWired | EmbedAttempt::Failed { .. } => Ok(None),
            }
        };
        let (fts_rows, trigram_rows, vector_rows) = tokio::try_join!(
            Box::pin(self.arm_rows(options, ArmKind::Fts, tag_filter, query)),
            Box::pin(trigram),
            Box::pin(vector)
        )?;

        let mut fallback = None;
        let trigram_rows = match trigram_rows {
            Some(rows) => Some(rows),
            None => {
                if fts_rows.is_empty() && should_trigram_fallback(&options.text) {
                    let recovered = self
                        .arm_rows(options, ArmKind::Trigram, tag_filter, query)
                        .await?;
                    if !recovered.is_empty() {
                        fallback = Some(MemoryFallback::Trigram);
                    }
                    Some(recovered)
                } else {
                    None
                }
            }
        };

        let degraded = if let EmbedAttempt::Failed {
            failure,
            elapsed_ms,
        } = &embed
        {
            self.vector_dropped(failure, *elapsed_ms);
            Some(failure.reason())
        } else {
            None
        };

        let fused = fuse(&fts_rows, vector_rows.as_deref(), trigram_rows.as_deref());
        let top = fused.first().map(|entry| entry.hit.score);
        let hits: Vec<MemorySearchHit> = fused
            .into_iter()
            .filter(|entry| {
                top.is_some_and(|top| passes_fusion_gate(entry.arm_count, entry.hit.score, top))
            })
            .map(|entry| entry.hit)
            .collect();
        let mut hits = distinct(hits);
        hits.truncate(usize::try_from(options.limit.max(1))?);
        self.record_access(&hits)?;
        Ok(SearchOutcome {
            hits,
            degraded,
            fallback,
        })
    }

    fn record_access(&self, hits: &[MemorySearchHit]) -> Result<(), Error> {
        let messages = hit_uuids(hits, MemoryHitSource::Message)?;
        let summaries = hit_uuids(hits, MemoryHitSource::Summary)?;
        self.access.record(&self.pool, messages, summaries);
        Ok(())
    }
}

pub(crate) fn hit_uuids(
    hits: &[MemorySearchHit],
    source: MemoryHitSource,
) -> Result<Vec<Uuid>, Error> {
    hits.iter()
        .filter(|hit| hit.source == source)
        .map(|hit| {
            Uuid::parse_str(&hit.id).map_err(|error| {
                Error::protocol(format!("search hit id {} is not a uuid: {error}", hit.id))
            })
        })
        .collect()
}

const ITERATIVE_SCAN: &str = "SELECT set_config('hnsw.iterative_scan', 'strict_order', true) \
     WHERE (SELECT string_to_array(extversion, '.')::int[] FROM pg_extension WHERE extname = 'vector') \
           >= ARRAY[0, 8]";

pub(crate) async fn tune_hnsw(tx: &mut sqlx::PgConnection, ef_search: u64) -> Result<(), Error> {
    sqlx::query_scalar!(
        r#"select set_config('hnsw.ef_search', $1, true) as "ok!""#,
        ef_search.to_string()
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|source| Error::operation("vector search", source))?;
    sqlx::query(ITERATIVE_SCAN)
        .execute(&mut *tx)
        .await
        .map_err(|source| Error::operation("vector search", source))?;
    Ok(())
}
