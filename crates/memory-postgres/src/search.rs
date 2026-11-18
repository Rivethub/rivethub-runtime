use crate::embed::EmbedAttempt;
use crate::error::Error;
use jiff::Timestamp;
use kernel::memory::MemoryChunkMatch;
use kernel::memory::MemoryFallback;
use kernel::memory::MemoryHitSource;
use kernel::memory::MemoryScope;
use kernel::memory::SearchMode;
use kernel::memory::TagLiteral;
use kernel::memory::TagWire;
use kernel::memory::{SummaryKind, SummaryPassage};
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
    pub(crate) summary_index_key: Option<[u8; 32]>,
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
    pub passage: Option<SummaryPassage>,
    pub id: String,
    pub source: MemoryHitSource,
    pub content: String,
    pub created_at: Timestamp,
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

impl MemorySearchHit {
    pub(crate) fn detail_id(&self) -> &str {
        self.passage
            .as_ref()
            .map_or(&self.id, |passage| &passage.id)
    }
}

pub(crate) struct SearchRead<'a> {
    pub(crate) memory: &'a crate::PostgresMemory,
    pub(crate) tx: sqlx::Transaction<'static, sqlx::Postgres>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchOutcome {
    pub hits: Vec<MemorySearchHit>,
    pub degraded: Option<String>,
    pub fallback: Option<MemoryFallback>,
    pub literal_skipped: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ArmRow {
    pub(crate) chunk: Option<MemoryChunkMatch>,
    pub(crate) passage: Option<SummaryPassage>,
    pub(crate) record_id: Option<String>,
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
    pub(crate) newest: Option<DateTime<Utc>>,
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

fn fuse(
    fts: &[ArmRow],
    vector: Option<&[ArmRow]>,
    trigram: Option<&[ArmRow]>,
    own: &std::collections::BTreeSet<String>,
    own_weight: f64,
) -> Vec<FusedEntry> {
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
                    passage: row.passage.clone(),
                    id: row.record_id.clone().unwrap_or_else(|| row.id.clone()),
                    source: row.source,
                    content: String::new(),
                    role: row.role,
                    agent: row.agent.clone(),
                    kind: row.kind,
                    tool_name: row.tool_name.clone(),
                    conversation_id: row.conversation_id.clone(),
                    created_at: row.created_at,
                    score: fusion_score(ranked.rrf, row)
                        * if row.session_id.as_ref().is_some_and(|key| own.contains(key)) {
                            own_weight
                        } else {
                            1.0
                        },
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
            .then_with(|| left.hit.detail_id().cmp(right.hit.detail_id()))
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
                passage: row.passage,
                id: row.record_id.unwrap_or(row.id),
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
            hit.passage.is_some()
                || seen.insert((
                    hit.source == MemoryHitSource::Summary,
                    hit.session_id.clone(),
                    hit.created_at,
                    hit.content_key.clone(),
                ))
        })
        .collect()
}

impl crate::PostgresMemory {
    pub async fn search_outcome_for(
        &self,
        query: &kernel::memory::SearchQuery,
    ) -> Result<SearchOutcome, Error> {
        let (mut read, options, embed) = self.start_search(query).await?;
        let outcome = read.search_outcome(&options, embed).await?;
        read.finish(&outcome.hits).await?;
        Ok(outcome)
    }

    pub(crate) async fn start_search(
        &self,
        query: &kernel::memory::SearchQuery,
    ) -> Result<(SearchRead<'_>, SearchOptions, EmbedAttempt), Error> {
        if query.window.is_some() && query.since.is_none() && query.before.is_none() {
            return Err(Error::UnresolvedWindow);
        }
        let options = SearchOptions {
            summary_index_key: self.summary_index_key,
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
        }
        .without_nul();
        let embed = if matches!(options.mode, SearchMode::Trigram | SearchMode::Regex) {
            EmbedAttempt::NotWired
        } else {
            self.embed_query(&options.text).await
        };
        let tx = self
            .pool
            .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .await
            .map_err(|source| Error::operation("search snapshot", source))?;
        Ok((SearchRead { memory: self, tx }, options, embed))
    }

    fn record_access(&self, hits: &[MemorySearchHit]) -> Result<(), Error> {
        let messages = hit_uuids(hits, MemoryHitSource::Message)?;
        let summaries = hit_uuids(hits, MemoryHitSource::Summary)?;
        self.access.record(&self.pool, messages, summaries);
        Ok(())
    }
}

impl SearchRead<'_> {
    pub(crate) async fn finish(self, hits: &[MemorySearchHit]) -> Result<(), Error> {
        self.tx
            .commit()
            .await
            .map_err(|source| Error::operation("search snapshot", source))?;
        self.memory.record_access(hits)
    }

    pub(crate) async fn search_outcome(
        &mut self,
        options: &SearchOptions,
        embed: EmbedAttempt,
    ) -> Result<SearchOutcome, Error> {
        let tag_filter = options.tag.as_ref().map(TagFilter::from);
        let mut outcome = match options.mode {
            SearchMode::Hybrid => {
                self.hybrid_search(options, tag_filter.as_ref(), embed)
                    .await
            }
            SearchMode::Vector => {
                self.vector_search(options, tag_filter.as_ref(), embed)
                    .await
            }
            SearchMode::Fts | SearchMode::Trigram | SearchMode::Regex => {
                self.explicit_search(options, tag_filter.as_ref(), embed)
                    .await
            }
        }?;
        self.fill_contents(&mut outcome.hits, &options.text).await?;
        Ok(outcome)
    }

    async fn vector_search(
        &mut self,
        options: &SearchOptions,
        tag_filter: Option<&TagFilter>,
        embed: EmbedAttempt,
    ) -> Result<SearchOutcome, Error> {
        match embed {
            EmbedAttempt::NotWired => self.fts_explicit(options, tag_filter, None, None).await,
            EmbedAttempt::Embedded { vector, ef_search } => {
                let rows = self
                    .vector_arm_rows(options, tag_filter, &vector, ef_search)
                    .await?;
                let hits = finish_rows(rows, options.limit)?;
                Ok(SearchOutcome {
                    hits,
                    degraded: None,
                    fallback: None,
                    literal_skipped: false,
                })
            }
            EmbedAttempt::Failed {
                failure,
                elapsed_ms,
            } => {
                self.memory.vector_dropped(&failure, elapsed_ms);
                let reason = failure.reason();
                self.fts_explicit(options, tag_filter, None, Some(reason))
                    .await
            }
        }
    }

    async fn explicit_search(
        &mut self,
        options: &SearchOptions,
        tag_filter: Option<&TagFilter>,
        embed: EmbedAttempt,
    ) -> Result<SearchOutcome, Error> {
        if options.mode != SearchMode::Fts {
            let kind = match options.mode {
                SearchMode::Regex => ArmKind::Regex,
                SearchMode::Hybrid | SearchMode::Fts | SearchMode::Vector | SearchMode::Trigram => {
                    ArmKind::Trigram
                }
            };
            let rows = self.arm_rows(options, kind, tag_filter, None).await?;
            let hits = finish_rows(rows, options.limit)?;
            return Ok(SearchOutcome {
                hits,
                degraded: None,
                fallback: None,
                literal_skipped: false,
            });
        }
        let mut degraded = None;
        let query = match embed {
            EmbedAttempt::NotWired => None,
            EmbedAttempt::Embedded { vector, .. } => Some(vector),
            EmbedAttempt::Failed {
                failure,
                elapsed_ms,
            } => {
                self.memory.vector_warn_only(&failure, elapsed_ms);
                degraded = Some(failure.reason());
                None
            }
        };
        self.fts_explicit(options, tag_filter, query, degraded)
            .await
    }

    async fn fts_explicit(
        &mut self,
        options: &SearchOptions,
        tag_filter: Option<&TagFilter>,
        query: Option<Vec<f32>>,
        degraded: Option<String>,
    ) -> Result<SearchOutcome, Error> {
        let rows = self
            .arm_rows(options, ArmKind::FtsExplicit, tag_filter, query.as_deref())
            .await?;
        let mut literal_skipped = false;
        let hits = if rows.is_empty() && should_trigram_fallback(&options.text) {
            let recovered = self
                .optional_literal_rows(options, tag_filter, query.as_deref())
                .await?;
            literal_skipped = recovered.is_none();
            let recovered = said_first(recovered.unwrap_or_default());
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
        Ok(SearchOutcome {
            hits,
            degraded,
            fallback: None,
            literal_skipped,
        })
    }

    async fn hybrid_search(
        &mut self,
        options: &SearchOptions,
        tag_filter: Option<&TagFilter>,
        embed: EmbedAttempt,
    ) -> Result<SearchOutcome, Error> {
        let query = embed.vector().map(Vec::as_slice);
        let fts_rows = self
            .arm_rows(options, ArmKind::Fts, tag_filter, query)
            .await?;
        let trigram_rows = if literal_scan(&options.text) {
            Some(
                self.optional_literal_rows(options, tag_filter, query)
                    .await?,
            )
        } else {
            None
        };
        let vector_rows = match &embed {
            EmbedAttempt::Embedded { vector, ef_search } => Some(
                self.vector_arm_rows(options, tag_filter, vector, *ef_search)
                    .await?,
            ),
            EmbedAttempt::NotWired | EmbedAttempt::Failed { .. } => None,
        };

        let mut fallback = None;
        let mut literal_skipped = false;
        let trigram_rows = match trigram_rows {
            Some(scanned) => {
                literal_skipped = scanned.is_none();
                scanned
            }
            None => {
                if fts_rows.is_empty() && should_trigram_fallback(&options.text) {
                    let recovered = self
                        .optional_literal_rows(options, tag_filter, query)
                        .await?;
                    literal_skipped = recovered.is_none();
                    if recovered.as_ref().is_some_and(|rows| !rows.is_empty()) {
                        fallback = Some(MemoryFallback::Trigram);
                    }
                    recovered
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
            self.memory.vector_dropped(failure, *elapsed_ms);
            Some(failure.reason())
        } else {
            None
        };

        let fused = self
            .fuse_owned(&fts_rows, vector_rows.as_deref(), trigram_rows.as_deref())
            .await?;
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
        Ok(SearchOutcome {
            hits,
            degraded,
            fallback,
            literal_skipped,
        })
    }

    async fn fuse_owned(
        &mut self,
        fts: &[ArmRow],
        vector: Option<&[ArmRow]>,
        trigram: Option<&[ArmRow]>,
    ) -> Result<Vec<FusedEntry>, Error> {
        let keys = fts
            .iter()
            .chain(vector.into_iter().flatten())
            .chain(trigram.into_iter().flatten())
            .filter_map(|row| row.session_id.clone())
            .collect::<Vec<_>>();
        let owners = crate::team::team_owners(&mut *self.tx, &keys).await?;
        let own = if owners.is_empty() {
            std::collections::BTreeSet::new()
        } else {
            crate::team::own_sessions(&mut self.tx, &keys).await?
        };
        Ok(fuse(fts, vector, trigram, &own, self.memory.own_weight))
    }
}

pub(crate) fn whole_summary_ids(hits: &[MemorySearchHit]) -> Result<Vec<Uuid>, Error> {
    hits.iter()
        .filter(|hit| hit.source == MemoryHitSource::Summary && hit.passage.is_none())
        .map(|hit| search_uuid(&hit.id))
        .collect()
}

pub(crate) fn passage_ids(hits: &[MemorySearchHit]) -> Result<Vec<Uuid>, Error> {
    hits.iter()
        .filter_map(|hit| hit.passage.as_ref())
        .map(|passage| search_uuid(&passage.id))
        .collect()
}

fn search_uuid(id: &str) -> Result<Uuid, Error> {
    Uuid::parse_str(id)
        .map_err(|error| Error::protocol(format!("search hit id {id} is not a uuid: {error}")))
}

pub(crate) fn hit_uuids(
    hits: &[MemorySearchHit],
    source: MemoryHitSource,
) -> Result<Vec<Uuid>, Error> {
    hits.iter()
        .filter(|hit| hit.source == source)
        .map(|hit| search_uuid(&hit.id))
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

#[cfg(test)]
#[path = "../tests/unit/recall_rank.rs"]
mod recall_rank;
