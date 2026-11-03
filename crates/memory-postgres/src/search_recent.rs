use pgvector::HalfVector;
use sqlx::QueryBuilder;

use crate::error::Error;
use crate::search::ArmRow;
use crate::search_sql::ArmContext;
use crate::search_sql::ArmSqlRow;
use crate::search_sql::LiteralText;
use crate::search_sql::push_embedded_semantic;
use crate::search_sql::push_held;
use crate::search_sql::push_scope_conditions;
use crate::search_sql::push_select_prefix;
use crate::search_sql::push_time_conditions;
use crate::search_sql::recency_columns;
use crate::search_sql::semantic_floor;

const RECENT_MESSAGES: i64 = 8_000;

impl crate::PostgresMemory {
    pub(crate) async fn recent_literals(
        &self,
        context: &ArmContext<'_>,
        tokens: &[String],
        query_vector: Option<&[f32]>,
        pool: i64,
    ) -> Result<Option<Vec<ArmRow>>, Error> {
        let mut query = QueryBuilder::new("");
        push_select_prefix(&mut query, true, false);
        query.push("1.0::float8 AS score, ");
        if let Some(vector) = query_vector.map(HalfVector::from_f32_slice) {
            push_embedded_semantic(&mut query, "m.embedding", "m", &vector);
        } else {
            query.push(semantic_floor("m"));
            query.push("::float8 AS semantic, ");
        }
        query.push(format!(
            "{} FROM (SELECT * FROM rh_messages ORDER BY created_at DESC LIMIT {RECENT_MESSAGES}) m WHERE ",
            recency_columns("m", true)
        ));
        if let Some(filter) = context.tag_filter {
            filter.push_condition(&mut query, true);
        } else {
            query.push("true");
        }
        let text = LiteralText::of(context.options, true);
        for token in tokens {
            query.push(" AND ");
            push_held(&mut query, text, token);
        }
        push_time_conditions(&mut query, context.options, "m");
        push_scope_conditions(&mut query, context.options, true);
        query.push(format!(" ORDER BY m.created_at DESC LIMIT {pool}"));
        let rows = query
            .build_query_as::<ArmSqlRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("search", source))?;
        if i64::try_from(rows.len()).is_ok_and(|found| found < pool) {
            return Ok(None);
        }
        rows.into_iter()
            .map(|row| row.into_arm(true))
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }
}
