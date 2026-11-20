use crate::backfills::Backfill;
use crate::error::Error;
use crate::search::ArmRow;
use crate::search::TagFilter;
use crate::search::{SearchOptions, SearchRead};
use crate::search_sql::ArmContext;
use crate::search_sql::push_embedded_semantic;
use crate::search_sql::push_scope_conditions;
use crate::search_sql::push_select_prefix;
use crate::search_sql::push_time_conditions;
use crate::search_sql::recency_columns;
use crate::search_sql::semantic_floor;
use crate::search_sql::{ArmSqlRow, SearchTable};
use kernel::memory::MemoryScope;
use kernel::memory_core::hybrid_pool;
use pgvector::HalfVector;
use sqlx::QueryBuilder;
use std::collections::BTreeMap;
use strum::IntoEnumIterator as _;
use wire::message::Role;

const CHUNK_POOL_MULTIPLIER: i64 = 4;
const CHUNK_POOL_MAX: i64 = 400;

fn chunk_pool(limit: u32) -> i64 {
    (hybrid_pool(limit) * CHUNK_POOL_MULTIPLIER).min(CHUNK_POOL_MAX)
}

fn said_only(options: &SearchOptions) -> bool {
    Role::iter()
        .filter(|role| options.admits(*role))
        .all(|role| match role {
            Role::User | Role::Assistant => true,
            Role::System | Role::Tool => false,
        })
}

impl SearchRead<'_> {
    pub(crate) async fn vector_arm_rows(
        &mut self,
        options: &SearchOptions,
        tag_filter: Option<&TagFilter>,
        query_vector: &[f32],
        ef_search: u64,
    ) -> Result<Vec<ArmRow>, Error> {
        use SearchTable::{Legacy, Message};
        let vector = HalfVector::from_f32_slice(query_vector);
        let context = ArmContext {
            options,
            tag_filter,
            patience: crate::search_literal::Patience::Asked,
        };
        let messages = matches!(options.scope, MemoryScope::Messages | MemoryScope::Both);
        let summaries = matches!(options.scope, MemoryScope::Summaries | MemoryScope::Both);
        crate::search::tune_hnsw(&mut self.tx, ef_search).await?;
        let mut rows = if messages {
            let parents = vector_table(&mut self.tx, &context, &vector, Message).await?;
            let chunks = self.chunk_arm_rows(&context, &vector).await?;
            merge_chunk_arm(parents, chunks)
        } else {
            Vec::new()
        };
        if summaries {
            let mut summary_rows = vector_table(&mut self.tx, &context, &vector, Legacy).await?;
            let mut passages = passage_vector(&mut self.tx, &context, &vector).await?;
            if !passages.is_empty() {
                summary_rows.append(&mut passages);
                summary_rows.sort_by(|a, b| b.arm_score.total_cmp(&a.arm_score));
                summary_rows.truncate(usize::try_from(hybrid_pool(options.limit))?);
            }
            rows.extend(summary_rows);
        }
        rows.sort_by(|a, b| {
            b.arm_score
                .total_cmp(&a.arm_score)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(rows)
    }

    async fn chunk_arm_rows(
        &mut self,
        context: &ArmContext<'_>,
        vector: &HalfVector,
    ) -> Result<Vec<ArmRow>, Error> {
        use SearchTable::Message;
        let mut query = QueryBuilder::new("");
        push_select_prefix(&mut query, Message, true);
        query.push(format!(
            "NULL::timestamptz AS newest, coalesce(1.0 - c.distance, 0.0)::float8 AS score, \
             coalesce(1.0 - c.distance, {})::float8 AS semantic, {} \
             FROM (SELECT DISTINCT ON (top.message_id) top.* FROM (\
             SELECT c.message_id, c.idx, c.char_start, c.char_end, c.content, c.embedding <=> ",
            semantic_floor("m"),
            recency_columns(Message)
        ));
        query.push_bind(vector.clone());
        query.push(
            " AS distance FROM rh_message_chunks c \
             JOIN rh_messages m ON m.id = c.message_id WHERE c.embedding IS NOT NULL",
        );
        if said_only(context.options)
            && self
                .memory
                .backfills
                .done(&mut *self.tx, Backfill::ChunkSaid)
                .await?
        {
            query.push(" AND c.said");
        }
        if let Some(filter) = context.tag_filter {
            query.push(" AND ");
            filter.push_condition(&mut query, true);
        }
        push_time_conditions(&mut query, context.options, Message);
        push_scope_conditions(&mut query, context.options, true);
        query.push(" ORDER BY c.embedding <=> ");
        query.push_bind(vector.clone());
        query.push(format!(
            " LIMIT {}) top ORDER BY top.message_id, top.distance) c \
             JOIN rh_messages m ON m.id = c.message_id",
            chunk_pool(context.options.limit)
        ));
        let fetched = query
            .build_query_as::<ArmSqlRow>()
            .fetch_all(&mut *self.tx)
            .await
            .map_err(|source| Error::operation("chunk search", source))?;
        fetched.into_iter().map(|row| row.into_arm(true)).collect()
    }
}

async fn vector_table(
    tx: &mut sqlx::PgConnection,
    context: &ArmContext<'_>,
    vector: &HalfVector,
    table: SearchTable,
) -> Result<Vec<ArmRow>, Error> {
    let options = context.options;
    let tag_filter = context.tag_filter;
    let is_message = table.is_message();
    let alias = table.alias();
    let from = table.from();
    let distance = format!("{alias}.embedding");
    let mut query = QueryBuilder::new("");
    push_select_prefix(&mut query, table, false);
    query.push("NULL::timestamptz AS newest, ");
    query.push("coalesce(1.0 - (");
    query.push(distance.clone());
    query.push(" <=> ");
    query.push_bind(vector.clone());
    query.push("), 0.0)::float8 AS score, ");
    push_embedded_semantic(&mut query, &distance, alias, vector);
    query.push(format!(
        "{} FROM {from} WHERE {alias}.embedding IS NOT NULL",
        recency_columns(table)
    ));
    if let Some(filter) = tag_filter {
        query.push(" AND ");
        filter.push_condition(&mut query, is_message);
    }
    table.push_eligibility(&mut query);
    push_time_conditions(&mut query, options, table);
    push_scope_conditions(&mut query, options, is_message);
    query.push(" ORDER BY ");
    query.push(distance);
    query.push(" <=> ");
    query.push_bind(vector.clone());
    query.push(format!(" LIMIT {}", hybrid_pool(options.limit)));
    let fetched = query
        .build_query_as::<ArmSqlRow>()
        .fetch_all(&mut *tx)
        .await
        .map_err(|source| Error::operation("vector search", source))?;
    fetched
        .into_iter()
        .map(|row| row.into_arm(is_message))
        .collect()
}

async fn passage_vector(
    tx: &mut sqlx::PgConnection,
    context: &ArmContext<'_>,
    vector: &HalfVector,
) -> Result<Vec<ArmRow>, Error> {
    let limit = usize::try_from(hybrid_pool(context.options.limit))?;
    let mut rows = passage_candidates(tx, context, vector, PassageScan::Indexed).await?;
    if rows.len() < limit {
        rows = passage_candidates(tx, context, vector, PassageScan::Exact).await?;
    }
    rows.into_iter().map(|row| row.into_arm(false)).collect()
}

#[derive(Clone, Copy)]
enum PassageScan {
    Indexed,
    Exact,
}

impl PassageScan {
    const fn distance_suffix(self) -> &'static str {
        match self {
            Self::Indexed => ")",
            Self::Exact => ") + 0",
        }
    }

    const fn filter(self) -> &'static str {
        match self {
            Self::Indexed => {
                "WHERE p.embedding IS NOT NULL AND EXISTS (
                SELECT 1 FROM rh_summaries s WHERE s.id = p.record_id"
            }
            Self::Exact => {
                "JOIN rh_summaries s ON s.id = p.record_id WHERE p.embedding IS NOT NULL"
            }
        }
    }
}

async fn passage_candidates(
    tx: &mut sqlx::PgConnection,
    context: &ArmContext<'_>,
    vector: &HalfVector,
    scan: PassageScan,
) -> Result<Vec<ArmSqlRow>, Error> {
    use SearchTable::Passage;
    let options = context.options;
    let mut query = QueryBuilder::new(
        "WITH candidates AS MATERIALIZED (
            SELECT p.id, p.record_id, p.ordinal, p.latest_at, p.content, (p.embedding <=> ",
    );
    query.push_bind(vector.clone());
    query.push(scan.distance_suffix());
    query.push(" AS distance FROM rh_summary_passages p ");
    query.push(scan.filter());
    Passage.push_eligibility(&mut query);
    push_passage_scope(&mut query, context);
    if matches!(scan, PassageScan::Indexed) {
        query.push(" OFFSET 0)");
    }
    push_time_conditions(&mut query, options, Passage);
    query.push(format!(
        " ORDER BY distance LIMIT {}) ",
        hybrid_pool(options.limit)
    ));
    push_select_prefix(&mut query, Passage, false);
    query.push(format!(
        "NULL::timestamptz AS newest, coalesce(1.0 - p.distance, 0.0)::float8 AS score,
         coalesce(1.0 - p.distance, {})::float8 AS semantic, {}
         FROM candidates p JOIN rh_summaries s ON s.id = p.record_id ORDER BY p.distance",
        semantic_floor("p"),
        recency_columns(Passage)
    ));
    query
        .build_query_as::<ArmSqlRow>()
        .fetch_all(tx)
        .await
        .map_err(|source| Error::operation("summary passage vector search", source))
}

fn push_passage_scope(query: &mut QueryBuilder<sqlx::Postgres>, context: &ArmContext<'_>) {
    let options = context.options;
    query.push(" AND p.index_key = s.index_key");
    if let Some(key) = &options.summary_index_key {
        query.push(" AND s.index_key = ").push_bind(key.as_slice());
    }
    if let Some(filter) = context.tag_filter {
        query.push(" AND ");
        filter.push_condition(query, false);
    }
    push_scope_conditions(query, options, false);
}

fn merge_chunk_arm(parents: Vec<ArmRow>, chunks: Vec<ArmRow>) -> Vec<ArmRow> {
    let mut merged: BTreeMap<String, ArmRow> = parents
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect();
    for chunk in chunks {
        if merged
            .get(&chunk.id)
            .is_none_or(|current| current.arm_score <= chunk.arm_score)
        {
            merged.insert(chunk.id.clone(), chunk);
        }
    }
    merged.into_values().collect()
}
