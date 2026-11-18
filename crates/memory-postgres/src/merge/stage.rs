use futures::StreamExt;
use sqlx::PgConnection;

use crate::PostgresMemory;
use crate::error::Error;

const COLUMNS: &str = "SELECT a.attname::text FROM pg_attribute a
     WHERE a.attrelid = to_regclass($1) AND a.attnum > 0 AND NOT a.attisdropped AND a.attgenerated = ''
     ORDER BY a.attnum";

const NEWEST: &str = "SELECT max(name) FROM _rivetos_migrations";

const EMBEDDED: [(&str, &str); 4] = [
    (
        "rh_messages",
        "embed_status = NULL, embed_error = NULL, embed_failures = 0",
    ),
    (
        "rh_message_chunks",
        "embed_status = NULL, embed_error = NULL, embed_failures = 0",
    ),
    (
        "rh_summaries",
        "embed_status = NULL, embed_error = NULL, embed_failures = 0",
    ),
    (
        "rh_wiki_topics",
        "embed_status = NULL, embed_error = NULL, embed_failures = 0",
    ),
];

pub(super) fn staged(table: &str) -> String {
    format!("merge_{table}")
}

async fn newest(conn: &mut PgConnection) -> Result<Option<String>, Error> {
    sqlx::query_scalar(NEWEST)
        .fetch_one(&mut *conn)
        .await
        .map_err(|error| Error::operation("merge schema", error))
}

pub(super) async fn same_schema(
    source: &mut PgConnection,
    target: &mut PgConnection,
) -> Result<(), Error> {
    let from = newest(source).await?;
    let into = newest(target).await?;
    if from == into {
        Ok(())
    } else {
        Err(Error::Merge(format!(
            "the source is at {} and the family store at {}; migrate both to the same schema first",
            from.as_deref().unwrap_or("no schema"),
            into.as_deref().unwrap_or("no schema")
        )))
    }
}

async fn columns(conn: &mut PgConnection, table: &str) -> Result<Vec<String>, Error> {
    sqlx::query_scalar(COLUMNS)
        .bind(format!("public.{table}"))
        .fetch_all(&mut *conn)
        .await
        .map_err(|error| Error::operation("merge columns", error))
}

pub(super) async fn copy(
    source: &mut PgConnection,
    target: &mut PgConnection,
    table: &str,
) -> Result<(u64, Vec<String>), Error> {
    let present = columns(source, table).await?;
    let wanted: Vec<String> = columns(target, table)
        .await?
        .into_iter()
        .filter(|column| present.contains(column))
        .collect();
    let stage = staged(table);
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TEMP TABLE {stage} (LIKE public.{table}) ON COMMIT DROP"
    )))
    .execute(&mut *target)
    .await
    .map_err(|error| Error::operation("merge stage", error))?;
    if wanted.is_empty() {
        return Ok((0, wanted));
    }
    let list = wanted
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut rows = source
        .copy_out_raw(&format!(
            "COPY (SELECT {list} FROM public.{table}) TO STDOUT"
        ))
        .await
        .map_err(|error| Error::operation("merge read", error))?;
    let mut sink = target
        .copy_in_raw(&format!("COPY {stage} ({list}) FROM STDIN"))
        .await
        .map_err(|error| Error::operation("merge stage", error))?;
    while let Some(chunk) = rows.next().await {
        let chunk = chunk.map_err(|error| Error::operation("merge read", error))?;
        sink.send(chunk)
            .await
            .map_err(|error| Error::operation("merge stage", error))?;
    }
    let rows = sink
        .finish()
        .await
        .map_err(|error| Error::operation("merge stage", error))?;
    Ok((rows, wanted))
}

async fn dimensions(memory: &PostgresMemory) -> Result<Option<i32>, Error> {
    sqlx::query_scalar(
        "SELECT vector_dims(embedding::vector) FROM rh_messages WHERE embedding IS NOT NULL LIMIT 1",
    )
    .fetch_optional(&memory.pool)
    .await
    .map_err(|error| Error::operation("merge embeddings", error))
}

pub(super) async fn align_embeddings(
    source: &PostgresMemory,
    target: &PostgresMemory,
    conn: &mut PgConnection,
) -> Result<bool, Error> {
    let (from, into) = (dimensions(source).await?, dimensions(target).await?);
    if from.is_none() || into.is_none() || from == into {
        return Ok(false);
    }
    for (table, status) in EMBEDDED {
        let stage = staged(table);
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {stage} SET embedding = NULL, {status}"
        )))
        .execute(&mut *conn)
        .await
        .map_err(|error| Error::operation("merge embeddings", error))?;
    }
    sqlx::query("UPDATE merge_rh_summary_passages SET embedding = NULL")
        .execute(&mut *conn)
        .await
        .map_err(|error| Error::operation("merge summary passage embeddings", error))?;
    Ok(true)
}

pub(super) async fn mark_summary_repairs(conn: &mut PgConnection) -> Result<(), Error> {
    sqlx::query(
        "UPDATE merge_rh_summaries s SET index_needs_repair = true
         WHERE s.kind IN ('session', 'part') AND NOT s.index_needs_repair
           AND NOT coalesce((
            SELECT bool_and(embedded AND char_start = previous_end AND ordinal = position)
                   AND max(char_end) = length(s.content)
            FROM (
                SELECT p.char_start, p.char_end, p.ordinal, p.embedding IS NOT NULL AS embedded,
                       lag(p.char_end, 1, 0) OVER (ORDER BY p.ordinal) AS previous_end,
                       row_number() OVER (ORDER BY p.ordinal) - 1 AS position
                FROM merge_rh_summary_passages p WHERE p.record_id = s.id
            ) pieces
         ), false)",
    )
    .execute(conn)
    .await
    .map_err(|error| Error::operation("merge summary repairs", error))?;
    Ok(())
}
