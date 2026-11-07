use kernel::memory_core::HYBRID_MIN_CONTENT_LEN;
use sqlx::QueryBuilder;

use crate::search_sql::ArmContext;
use crate::search_sql::alias_of;
use crate::search_sql::push_option_conditions;
use crate::search_sql::push_time_conditions;

const LENGTH_PREFIX: i64 = 1000;

fn long_enough(body: &str) -> String {
    format!(
        "(length(btrim(left({body}, {LENGTH_PREFIX}))) >= {HYBRID_MIN_CONTENT_LEN} \
         OR (octet_length({body}) > {LENGTH_PREFIX} AND length(btrim({body})) >= {HYBRID_MIN_CONTENT_LEN}))"
    )
}

pub(crate) fn push_fts_match(
    query: &mut QueryBuilder<sqlx::Postgres>,
    context: &ArmContext<'_>,
    is_message: bool,
    long: bool,
) {
    let alias = if is_message { "m" } else { "s" };
    query.push(" WHERE ");
    if let Some(filter) = context.tag_filter {
        filter.push_condition(query, is_message);
    } else {
        query.push("true");
    }
    query.push(format!(
        " AND {alias}.content_tsv @@ plainto_tsquery('english', "
    ));
    query.push_bind(context.options.text.clone());
    query.push(")");
    if long {
        query.push(" AND ");
        query.push(long_enough(if is_message {
            "CASE WHEN m.role = 'tool' THEN coalesce(m.tool_result, '') ELSE m.content END"
        } else {
            "s.content"
        }));
    }
    push_time_conditions(query, context.options, alias);
    push_option_conditions(query, context.options, is_message);
}

pub(crate) fn push_fts_where(
    query: &mut QueryBuilder<sqlx::Postgres>,
    context: &ArmContext<'_>,
    is_message: bool,
    long: bool,
) {
    push_fts_match(query, context, is_message, long);
    query.push(crate::session_marks::not_trashed(alias_of(is_message)));
}
