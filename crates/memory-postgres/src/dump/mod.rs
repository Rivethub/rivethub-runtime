mod filter;
mod parse;
mod relation;
mod sql;

use jiff::Timestamp;
use std::collections::BTreeMap;
use std::io::Write;
use wire::time::TimestampFormat as _;

use futures::StreamExt;
use sqlx::PgConnection;
use tokio_util::sync::CancellationToken;

use crate::PostgresMemory;
use crate::error::Error;
use relation::Relation;

pub use kernel::memory::DumpOrigin;
pub use kernel::memory::DumpSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopySource<'a> {
    pub relation: &'a str,
    pub compatibility: &'a str,
    pub columns: &'a [&'a str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedDump {
    pub source: Option<String>,
    pub host: Option<String>,
    pub rows: BTreeMap<String, u64>,
    pub summary_records_needing_reembedding: u64,
}

impl PostgresMemory {
    pub async fn export_memory_dump(
        &self,
        tables: &[CopySource<'_>],
        since: Option<Timestamp>,
        origin: &DumpOrigin,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        write_header(&mut out, origin)?;
        let mut conn = begin(&self.pool).await?;
        let exported = export_tables(&mut conn, &mut out, tables, since, cancellation).await;
        match exported {
            Ok(()) => commit(&mut conn, true).await?,
            Err(error) => return Err(rollback(&mut conn, error).await),
        }
        out.extend_from_slice(b"-- PostgreSQL database dump complete\n");
        Ok(out)
    }

    pub async fn import_memory_dump(
        &self,
        dump: &str,
        dry_run: bool,
        cancellation: &CancellationToken,
    ) -> Result<ImportedDump, Error> {
        let parsed = parse::parse(dump)?;
        let mut conn = begin(&self.pool).await?;
        let imported = import_tables(
            &mut conn,
            &parsed,
            cancellation,
            self.summary_index_key.as_ref(),
        )
        .await;
        match imported {
            Ok((rows, summary_records_needing_reembedding)) => {
                commit(&mut conn, !dry_run).await?;
                Ok(ImportedDump {
                    source: parsed.source,
                    host: parsed.host,
                    rows,
                    summary_records_needing_reembedding,
                })
            }
            Err(error) => Err(rollback(&mut conn, error).await),
        }
    }
}

async fn export_tables(
    conn: &mut PgConnection,
    out: &mut Vec<u8>,
    tables: &[CopySource<'_>],
    since: Option<Timestamp>,
    cancellation: &CancellationToken,
) -> Result<(), Error> {
    let mark = since.map(timestamp_mark).transpose()?;
    for table in tables {
        cancelled(cancellation)?;
        write_copy(conn, out, table, mark.as_deref()).await?;
    }
    Ok(())
}

async fn write_copy(
    conn: &mut PgConnection,
    out: &mut Vec<u8>,
    table: &CopySource<'_>,
    mark: Option<&str>,
) -> Result<(), Error> {
    ident(table.relation)?;
    ident(table.compatibility)?;
    for column in table.columns {
        ident(column)?;
    }
    let order = filter::order(table.relation)?;
    ident_list(order)?;
    let body = mark
        .map(|mark| filter::predicate(table.relation, mark))
        .transpose()?;
    let sql = copy_out(table.relation, table.columns, order, body.as_deref());
    writeln!(
        out,
        "COPY public.{} ({}) FROM stdin;",
        table.compatibility,
        table.columns.join(", ")
    )
    .map_err(|source| Error::DumpMessage(source.to_string()))?;
    let bytes = copy_bytes(conn, &sql).await?;
    out.extend_from_slice(&bytes);
    out.extend_from_slice(b"\\.\n");
    Ok(())
}

fn copy_out(relation: &str, columns: &[&str], order: &str, body: Option<&str>) -> String {
    let access = match relation {
        "rh_messages" => Some(("rh_message_access", "message_id")),
        "rh_summaries" => Some(("rh_summary_access", "summary_id")),
        _ => None,
    };
    let cols = columns
        .iter()
        .map(|column| match (access, *column) {
            (Some(_), "access_count" | "last_accessed_at") => {
                format!("coalesce(access.{column}, {relation}.{column}) AS {column}")
            }
            _ => format!("{relation}.{column}"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let from = match access {
        Some((table, key)) => {
            format!("{relation} LEFT JOIN {table} access ON access.{key} = {relation}.id")
        }
        None => relation.to_owned(),
    };
    match body {
        Some(body) => {
            format!("COPY (SELECT {cols} FROM {from} WHERE {body} ORDER BY {order}) TO STDOUT")
        }
        None => format!("COPY (SELECT {cols} FROM {from} ORDER BY {order}) TO STDOUT"),
    }
}

async fn copy_bytes(conn: &mut PgConnection, sql: &str) -> Result<Vec<u8>, Error> {
    let mut stream = conn
        .copy_out_raw(sql)
        .await
        .map_err(|source| Error::operation("export copy", source))?;
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|source| Error::operation("export copy", source))?;
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn import_tables(
    conn: &mut PgConnection,
    parsed: &parse::Parsed,
    cancellation: &CancellationToken,
    index_key: Option<&[u8; 32]>,
) -> Result<(BTreeMap<String, u64>, u64), Error> {
    sqlx::query("SET LOCAL rivet.defer_embed_enqueue = 'on'")
        .execute(&mut *conn)
        .await
        .map_err(|source| Error::operation("import defer", source))?;
    for relation in Relation::ALL {
        ensure_empty(conn, relation).await?;
    }
    let mut rows = BTreeMap::new();
    for relation in Relation::ALL {
        rows.insert(relation.table_name().to_owned(), 0);
    }
    let mut blocks: Vec<_> = parsed.blocks.iter().collect();
    blocks.sort_by_key(|block| block.relation);
    for block in blocks {
        cancelled(cancellation)?;
        let count = if block.relation == Relation::Summaries {
            copy_summaries(conn, block).await?
        } else {
            copy_relation(conn, block).await?
        };
        rows.insert(block.relation.table_name().to_owned(), count);
    }
    validate_summary_graph(conn).await?;
    mark_summary_index_repairs(conn, index_key).await?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM rh_summaries WHERE kind IN ('session','part') AND index_needs_repair",
    )
    .fetch_one(conn)
    .await
    .map_err(|source| Error::operation("count summary index repairs", source))?;
    Ok((rows, u64::try_from(pending)?))
}

pub(crate) async fn validate_summary_graph(conn: &mut PgConnection) -> Result<(), Error> {
    let invalid: bool = sqlx::query_scalar(sql::SUMMARY_GRAPH)
        .fetch_one(&mut *conn)
        .await
        .map_err(|source| Error::operation("validate summary graph", source))?;
    if invalid {
        return Err(Error::DumpMessage(
            "incomplete or inconsistent top-down summary graph".into(),
        ));
    }
    Ok(())
}

async fn mark_summary_index_repairs(
    conn: &mut PgConnection,
    key: Option<&[u8; 32]>,
) -> Result<(), Error> {
    let key = key.map(<[_; 32]>::as_slice);
    for statement in [
        "UPDATE rh_summary_passages SET embedding = NULL
         WHERE embedding IS NOT NULL AND index_key IS DISTINCT FROM $1",
        "UPDATE rh_summaries SET index_key = $1
         WHERE kind IN ('session', 'part') AND index_key IS DISTINCT FROM $1",
    ] {
        sqlx::query(statement)
            .bind(key)
            .execute(&mut *conn)
            .await
            .map_err(|error| Error::operation("import summary embedding specification", error))?;
    }
    sqlx::query(
        "UPDATE rh_summaries s SET index_needs_repair = true
         WHERE s.kind IN ('session', 'part') AND NOT s.index_needs_repair
           AND NOT rh_summary_record_indexed(s.id)",
    )
    .execute(conn)
    .await
    .map_err(|source| Error::operation("mark summary index repairs", source))?;
    Ok(())
}

async fn copy_relation(conn: &mut PgConnection, block: &parse::Block) -> Result<u64, Error> {
    let relation = block.relation.rh();
    let stage = format!("dump_{relation}");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TEMP TABLE {stage} (LIKE public.{relation} INCLUDING DEFAULTS) ON COMMIT DROP"
    )))
    .execute(&mut *conn)
    .await
    .map_err(|source| Error::operation("stage dump relation", source))?;
    copy_in(conn, &stage, block).await?;
    let columns = block.columns.join(", ");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO public.{relation} ({columns}) SELECT {columns} FROM {stage}"
    )))
    .execute(conn)
    .await
    .map(|done| done.rows_affected())
    .map_err(|source| Error::operation("load dump relation", source))
}

async fn copy_summaries(conn: &mut PgConnection, block: &parse::Block) -> Result<u64, Error> {
    sqlx::raw_sql(
        "CREATE TEMP TABLE dump_summaries (LIKE rh_summaries INCLUDING DEFAULTS) ON COMMIT DROP;
         ALTER TABLE dump_summaries ALTER COLUMN owner_user_id DROP NOT NULL,
             ALTER COLUMN embedding TYPE text",
    )
    .execute(&mut *conn)
    .await
    .map_err(|source| Error::operation("stage dump summaries", source))?;
    copy_in(conn, "dump_summaries", block).await?;

    let mut columns: Vec<_> = block.columns.iter().map(String::as_str).collect();
    for required in ["kind", "owner_user_id"] {
        if !columns.contains(&required) {
            columns.push(required);
        }
    }
    let depth_kind = "CASE WHEN s.depth = 0 THEN 'leaf' WHEN s.depth = 1 THEN 'branch'
                           WHEN s.depth >= 2 THEN 'root' END";
    let expressions = columns
        .iter()
        .map(|column| match *column {
            "kind" if !block.columns.iter().any(|name| name == "kind") => depth_kind.to_owned(),
            "kind" => format!(
                "CASE WHEN s.pipeline_version < 7 AND s.kind NOT IN
                 ('leaf', 'branch', 'root', 'session', 'part', 'section')
                 THEN {depth_kind} ELSE s.kind END"
            ),
            "owner_user_id" => "coalesce(s.owner_user_id, c.owner_user_id)".to_owned(),
            "embedding" => {
                "CASE WHEN s.pipeline_version < 7 THEN s.embedding::halfvec END".to_owned()
            }
            name => format!("s.{name}"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO rh_summaries ({}) SELECT {expressions}
         FROM dump_summaries s LEFT JOIN rh_conversations c ON c.id = s.conversation_id",
        columns.join(", ")
    )))
    .execute(conn)
    .await
    .map(|done| done.rows_affected())
    .map_err(|source| Error::operation("load dump summaries", source))
}

async fn copy_in(conn: &mut PgConnection, table: &str, block: &parse::Block) -> Result<u64, Error> {
    for column in &block.columns {
        ident(column)?;
    }
    let sql = format!("COPY {table} ({}) FROM STDIN", block.columns.join(", "));
    let mut copy = conn
        .copy_in_raw(&sql)
        .await
        .map_err(|source| Error::operation("import copy", source))?;
    if !block.data.is_empty()
        && let Err(source) = copy.send(block.data.as_slice()).await
    {
        return match copy.abort("import send failed").await {
            Ok(()) => Err(Error::operation("import copy", source)),
            Err(abort) => Err(Error::operation("import copy abort", abort)),
        };
    }
    copy.finish()
        .await
        .map_err(|source| Error::operation("import copy", source))
}

async fn ensure_empty(conn: &mut PgConnection, relation: Relation) -> Result<(), Error> {
    let row: CountRow = sqlx::query_as(relation.count_sql())
        .fetch_one(&mut *conn)
        .await
        .map_err(|source| Error::operation("import empty", source))?;
    if row.count > 0 {
        Err(Error::DumpOccupied {
            table: relation.rh(),
        })
    } else {
        Ok(())
    }
}

async fn begin(pool: &sqlx::PgPool) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>, Error> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|source| Error::operation("dump connection", source))?;
    sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *conn)
        .await
        .map_err(|source| Error::operation("dump transaction", source))?;
    Ok(conn)
}

async fn commit(conn: &mut PgConnection, apply: bool) -> Result<(), Error> {
    let statement = if apply { "COMMIT" } else { "ROLLBACK" };
    sqlx::query(statement)
        .execute(conn)
        .await
        .map_err(|source| Error::operation("dump transaction", source))?;
    Ok(())
}

async fn rollback(conn: &mut PgConnection, error: Error) -> Error {
    match sqlx::query("ROLLBACK").execute(&mut *conn).await {
        Ok(_) => error,
        Err(source) => Error::operation("dump rollback", source),
    }
}

fn write_header(out: &mut Vec<u8>, origin: &DumpOrigin) -> Result<(), Error> {
    if origin.host.is_empty() || origin.host.contains(['\n', '\r']) {
        return Err(Error::DumpMessage(
            "export host is empty or has a line break".into(),
        ));
    }
    writeln!(
        out,
        "-- RivetHub memory export\n-- source {}\n-- host {}",
        origin.source, origin.host
    )
    .map_err(|source| Error::DumpMessage(source.to_string()))
}

fn timestamp_mark(since: Timestamp) -> Result<String, Error> {
    let text = since.rfc3339_offset();
    if text.bytes().all(|byte| {
        byte.is_ascii_digit() || matches!(byte, b'T' | b'Z' | b':' | b'.' | b'+' | b'-')
    }) {
        Ok(format!("timestamptz '{text}'"))
    } else {
        Err(Error::DumpMessage("export timestamp is not RFC3339".into()))
    }
}

fn ident(name: &str) -> Result<(), Error> {
    if !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        Ok(())
    } else {
        Err(Error::DumpMessage(format!(
            "invalid export identifier {name}"
        )))
    }
}

fn ident_list(order: &str) -> Result<(), Error> {
    for piece in order.split([',', ' ']) {
        if piece.is_empty() || piece == "DESC" || piece == "ASC" {
            continue;
        }
        ident(piece)?;
    }
    Ok(())
}

fn cancelled(cancellation: &CancellationToken) -> Result<(), Error> {
    if cancellation.is_cancelled() {
        Err(Error::DumpCancelled)
    } else {
        Ok(())
    }
}

#[derive(Debug, sqlx::FromRow)]
struct CountRow {
    count: i64,
}
