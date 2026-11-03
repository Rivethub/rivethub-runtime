use crate::backfills::Backfill;
use crate::error::Error;
use crate::search::ArmRow;
use crate::search::SearchOptions;
use crate::search::TagFilter;
use crate::search_sql::ArmContext;
use crate::search_sql::ArmSqlRow;
use crate::search_sql::push_embedded_semantic;
use crate::search_sql::push_scope_conditions;
use crate::search_sql::push_select_prefix;
use crate::search_sql::push_time_conditions;
use crate::search_sql::recency_columns;
use crate::search_sql::semantic_floor;
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

#[derive(Debug, Clone, Copy)]
enum VectorPart {
    Messages,
    Chunks,
    Summaries,
}

impl crate::PostgresMemory {
    pub(crate) async fn vector_arm_rows(
        &self,
        options: &SearchOptions,
        tag_filter: Option<&TagFilter>,
        query_vector: &[f32],
        ef_search: u64,
    ) -> Result<Vec<ArmRow>, Error> {
        let vector = HalfVector::from_f32_slice(query_vector);
        let context = ArmContext {
            options,
            tag_filter,
        };
        let messages = matches!(options.scope, MemoryScope::Messages | MemoryScope::Both);
        let summaries = matches!(options.scope, MemoryScope::Summaries | MemoryScope::Both);
        let (message_rows, chunks, summary_rows) = tokio::try_join!(
            Box::pin(self.vector_part(
                &context,
                &vector,
                VectorPart::Messages,
                messages,
                ef_search
            )),
            Box::pin(self.vector_part(&context, &vector, VectorPart::Chunks, messages, ef_search)),
            Box::pin(self.vector_part(
                &context,
                &vector,
                VectorPart::Summaries,
                summaries,
                ef_search
            )),
        )?;
        let mut rows = merge_chunk_arm(message_rows, chunks);
        rows.extend(summary_rows);
        rows.sort_by(|a, b| {
            b.arm_score
                .total_cmp(&a.arm_score)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(rows)
    }

    async fn vector_part(
        &self,
        context: &ArmContext<'_>,
        vector: &HalfVector,
        part: VectorPart,
        wanted: bool,
        ef_search: u64,
    ) -> Result<Vec<ArmRow>, Error> {
        if !wanted {
            return Ok(Vec::new());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|source| Error::operation("vector search", source))?;
        crate::search::tune_hnsw(&mut tx, ef_search).await?;
        match part {
            VectorPart::Messages => self.vector_table(context, vector, true, &mut tx).await,
            VectorPart::Chunks => self.chunk_arm_rows(context, vector, &mut tx).await,
            VectorPart::Summaries => self.vector_table(context, vector, false, &mut tx).await,
        }
    }

    async fn vector_table(
        &self,
        context: &ArmContext<'_>,
        vector: &HalfVector,
        is_message: bool,
        tx: &mut sqlx::PgConnection,
    ) -> Result<Vec<ArmRow>, Error> {
        let options = context.options;
        let tag_filter = context.tag_filter;
        let alias = if is_message { "m" } else { "s" };
        let table = if is_message {
            "rh_messages m"
        } else {
            "rh_summaries s"
        };
        let distance = format!("{alias}.embedding");
        let mut query = QueryBuilder::new("");
        push_select_prefix(&mut query, is_message, false);
        query.push("coalesce(1.0 - (");
        query.push(distance.clone());
        query.push(" <=> ");
        query.push_bind(vector.clone());
        query.push("), 0.0)::float8 AS score, ");
        push_embedded_semantic(&mut query, &distance, alias, vector);
        query.push(format!(
            "{} FROM {table} WHERE {alias}.embedding IS NOT NULL",
            recency_columns(alias, is_message)
        ));
        if let Some(filter) = tag_filter {
            query.push(" AND ");
            filter.push_condition(&mut query, is_message);
        }
        push_time_conditions(&mut query, options, alias);
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

    async fn chunk_arm_rows(
        &self,
        context: &ArmContext<'_>,
        vector: &HalfVector,
        tx: &mut sqlx::PgConnection,
    ) -> Result<Vec<ArmRow>, Error> {
        let mut query = QueryBuilder::new("");
        push_select_prefix(&mut query, true, true);
        query.push(format!(
            "coalesce(1.0 - c.distance, 0.0)::float8 AS score, \
             coalesce(1.0 - c.distance, {})::float8 AS semantic, {} \
             FROM (SELECT DISTINCT ON (top.message_id) top.* FROM (\
             SELECT c.message_id, c.idx, c.char_start, c.char_end, c.content, c.embedding <=> ",
            semantic_floor("m"),
            recency_columns("m", true)
        ));
        query.push_bind(vector.clone());
        query.push(
            " AS distance FROM rh_message_chunks c \
             JOIN rh_messages m ON m.id = c.message_id WHERE c.embedding IS NOT NULL",
        );
        if said_only(context.options)
            && self.backfills.done(&self.pool, Backfill::ChunkSaid).await?
        {
            query.push(" AND c.said");
        }
        if let Some(filter) = context.tag_filter {
            query.push(" AND ");
            filter.push_condition(&mut query, true);
        }
        push_time_conditions(&mut query, context.options, "m");
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
            .fetch_all(&mut *tx)
            .await
            .map_err(|source| Error::operation("chunk search", source))?;
        fetched.into_iter().map(|row| row.into_arm(true)).collect()
    }
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
