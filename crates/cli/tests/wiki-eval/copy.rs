use std::collections::HashSet;

use futures::StreamExt;
use sqlx::PgConnection;
use sqlx::PgPool;
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::failure::Failure;
use crate::failure::database;
use crate::failure::note;

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct Counts {
    pub(crate) conversations: i64,
    pub(crate) messages: i64,
    pub(crate) summaries: i64,
    pub(crate) topics: i64,
    pub(crate) aliases: i64,
    pub(crate) placements: i64,
}

#[derive(Clone, Copy)]
enum Table {
    Topics,
    Aliases,
    Placements,
    Conversations,
    Messages,
    Summaries,
    Sources,
}

impl Table {
    fn name(self) -> &'static str {
        match self {
            Self::Topics => "rh_topics",
            Self::Aliases => "rh_topic_aliases",
            Self::Placements => "rh_session_placements",
            Self::Conversations => "rh_conversations",
            Self::Messages => "rh_messages",
            Self::Summaries => "rh_summaries",
            Self::Sources => "rh_summary_sources",
        }
    }
}

const TABLES: [Table; 7] = [
    Table::Topics,
    Table::Aliases,
    Table::Placements,
    Table::Conversations,
    Table::Messages,
    Table::Summaries,
    Table::Sources,
];

pub(crate) async fn copy_slice(
    app: &str,
    source: &str,
    principal: &str,
    ids: &[Uuid],
    secrets: &[String],
) -> Result<Counts, Failure> {
    if !principal_ok(principal) {
        return Err(Failure::Usage {
            detail: "principal must match [A-Za-z0-9_.-]+".to_owned(),
        });
    }
    if ids.is_empty() {
        return Err(Failure::Copy("the slice names no conversations".to_owned()));
    }
    note("copying the slice from the live database");
    let live = source_pool(source, principal, secrets).await?;
    let scratch = PgPoolOptions::new()
        .max_connections(1)
        .connect(app)
        .await
        .map_err(|error| database(&error, secrets))?;
    let copied = transfer(&live, &scratch, ids, secrets).await;
    live.close().await;
    scratch.close().await;
    copied
}

async fn source_pool(url: &str, principal: &str, secrets: &[String]) -> Result<PgPool, Failure> {
    let options: PgConnectOptions = url.parse().map_err(|error: sqlx::Error| Failure::Usage {
        detail: format!("source url: {error}"),
    })?;
    let options = options.application_name("wiki-eval");
    let principal = principal.to_owned();
    PgPoolOptions::new()
        .max_connections(1)
        .after_connect(move |conn, _| {
            let principal = principal.clone();
            Box::pin(async move {
                sqlx::query("SELECT set_config('rh.principal', $1, false)")
                    .bind(principal)
                    .execute(&mut *conn)
                    .await?;
                sqlx::query("SET default_transaction_read_only TO on")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .map_err(|error| database(&error, secrets))
}

async fn transfer(
    source: &PgPool,
    scratch: &PgPool,
    ids: &[Uuid],
    secrets: &[String],
) -> Result<Counts, Failure> {
    let mut from = source
        .acquire()
        .await
        .map_err(|error| database(&error, secrets))?;
    let mut into = scratch
        .acquire()
        .await
        .map_err(|error| database(&error, secrets))?;
    guarded(&mut from, &mut into, ids, secrets).await
}

async fn guarded(
    from: &mut PgConnection,
    into: &mut PgConnection,
    ids: &[Uuid],
    secrets: &[String],
) -> Result<Counts, Failure> {
    session(into, "SET session_replication_role = 'replica'", secrets).await?;
    session(
        into,
        "SELECT set_config('rivet.defer_embed_enqueue', 'on', false)",
        secrets,
    )
    .await?;
    let copied = copy_tables(from, into, ids, secrets).await;
    let role = session(into, "SET session_replication_role = 'origin'", secrets).await;
    let deferred = session(
        into,
        "SELECT set_config('rivet.defer_embed_enqueue', 'off', false)",
        secrets,
    )
    .await;
    copied?;
    role?;
    deferred?;
    counts(into, ids, secrets).await
}

async fn session(
    conn: &mut PgConnection,
    sql: &'static str,
    secrets: &[String],
) -> Result<(), Failure> {
    sqlx::query(sql)
        .execute(&mut *conn)
        .await
        .map(|_| ())
        .map_err(|error| database(&error, secrets))
}

async fn copy_tables(
    from: &mut PgConnection,
    into: &mut PgConnection,
    ids: &[Uuid],
    secrets: &[String],
) -> Result<(), Failure> {
    let list = uuid_array(ids);
    for table in TABLES {
        let rows = copy_table(from, into, table, &list, secrets).await?;
        note(&format!("{} {rows}", table.name()));
    }
    Ok(())
}

async fn copy_table(
    from: &mut PgConnection,
    into: &mut PgConnection,
    table: Table,
    ids: &str,
    secrets: &[String],
) -> Result<u64, Failure> {
    let columns = shared_columns(from, into, table.name(), secrets).await?;
    let source_cols = column_list(&columns, "src")?;
    let dest_cols = column_list(&columns, "")?;
    let select = select_sql(table, &source_cols, ids);
    pump(
        from,
        into,
        &format!("COPY ({select}) TO STDOUT"),
        &format!("COPY {} ({dest_cols}) FROM STDIN", table.name()),
        secrets,
    )
    .await
}

async fn shared_columns(
    from: &mut PgConnection,
    into: &mut PgConnection,
    table: &str,
    secrets: &[String],
) -> Result<Vec<String>, Failure> {
    let source = columns(from, table, secrets).await?;
    let target = columns(into, table, secrets).await?;
    let have: HashSet<&str> = source.iter().map(String::as_str).collect();
    let shared: Vec<String> = target
        .into_iter()
        .filter(|column| have.contains(column.as_str()))
        .collect();
    if shared.is_empty() {
        return Err(Failure::Copy(format!("{table} has no copyable columns")));
    }
    Ok(shared)
}

async fn columns(
    conn: &mut PgConnection,
    table: &str,
    secrets: &[String],
) -> Result<Vec<String>, Failure> {
    sqlx::query_scalar(COLUMNS)
        .bind(format!("public.{table}"))
        .fetch_all(&mut *conn)
        .await
        .map_err(|error| database(&error, secrets))
}

fn column_list(columns: &[String], prefix: &str) -> Result<String, Failure> {
    let mut parts = Vec::with_capacity(columns.len());
    for column in columns {
        let quoted = quote_ident(column)?;
        if prefix.is_empty() {
            parts.push(quoted);
        } else {
            parts.push(format!("{prefix}.{quoted}"));
        }
    }
    Ok(parts.join(", "))
}

fn select_sql(table: Table, columns: &str, ids: &str) -> String {
    match table {
        Table::Topics => topics_sql(columns, ids),
        Table::Aliases => aliases_sql(columns, ids),
        Table::Placements => placements_sql(columns, ids),
        Table::Conversations => {
            where_sql("rh_conversations", columns, &format!("src.id = ANY({ids})"))
        }
        Table::Messages => where_sql(
            "rh_messages",
            columns,
            &format!("src.conversation_id = ANY({ids})"),
        ),
        Table::Summaries => summaries_sql(columns, ids),
        Table::Sources => sources_sql(columns, ids),
    }
}

fn topics_sql(columns: &str, ids: &str) -> String {
    format!(
        "WITH RECURSIVE tree AS (
            SELECT t.id, t.parent_id, 0 AS depth
              FROM rh_topics t
              JOIN rh_session_placements p ON p.topic_id = t.id
              JOIN rh_conversations c ON c.session_key = p.session_id
             WHERE c.id = ANY({ids})
            UNION
            SELECT parent.id, parent.parent_id, tree.depth + 1
              FROM rh_topics parent
              JOIN tree ON parent.id = tree.parent_id
         ), ranked AS (
            SELECT DISTINCT ON (id) id, depth FROM tree ORDER BY id, depth DESC
         )
         SELECT {columns}
           FROM rh_topics src
           JOIN ranked r ON r.id = src.id
          ORDER BY r.depth DESC"
    )
}

fn aliases_sql(columns: &str, ids: &str) -> String {
    format!(
        "WITH RECURSIVE tree AS (
            SELECT t.id, t.parent_id
              FROM rh_topics t
              JOIN rh_session_placements p ON p.topic_id = t.id
              JOIN rh_conversations c ON c.session_key = p.session_id
             WHERE c.id = ANY({ids})
            UNION
            SELECT parent.id, parent.parent_id
              FROM rh_topics parent
              JOIN tree ON parent.id = tree.parent_id
         )
         SELECT {columns}
           FROM rh_topic_aliases src
          WHERE src.topic_id IN (SELECT id FROM tree)"
    )
}

fn placements_sql(columns: &str, ids: &str) -> String {
    format!(
        "SELECT {columns}
           FROM rh_session_placements src
           JOIN rh_conversations c ON c.session_key = src.session_id
          WHERE c.id = ANY({ids})"
    )
}

fn summaries_sql(columns: &str, ids: &str) -> String {
    format!(
        "WITH RECURSIVE tree AS (
            SELECT s.id, s.parent_id, 0 AS depth
              FROM rh_summaries s
             WHERE s.conversation_id = ANY({ids})
            UNION
            SELECT parent.id, parent.parent_id, tree.depth + 1
              FROM rh_summaries parent
              JOIN tree ON parent.id = tree.parent_id
         ), ranked AS (
            SELECT DISTINCT ON (id) id, depth FROM tree ORDER BY id, depth DESC
         )
         SELECT {columns}
           FROM rh_summaries src
           JOIN ranked r ON r.id = src.id
          ORDER BY r.depth DESC"
    )
}

fn sources_sql(columns: &str, ids: &str) -> String {
    format!(
        "WITH RECURSIVE tree AS (
            SELECT s.id, s.parent_id
              FROM rh_summaries s
             WHERE s.conversation_id = ANY({ids})
            UNION
            SELECT parent.id, parent.parent_id
              FROM rh_summaries parent
              JOIN tree ON parent.id = tree.parent_id
         )
         SELECT {columns}
           FROM rh_summary_sources src
          WHERE src.summary_id IN (SELECT id FROM tree)
            AND src.message_id IN (
                SELECT id FROM rh_messages WHERE conversation_id = ANY({ids})
            )"
    )
}

fn where_sql(table: &str, columns: &str, filter: &str) -> String {
    format!("SELECT {columns} FROM {table} src WHERE {filter}")
}

async fn pump(
    from: &mut PgConnection,
    into: &mut PgConnection,
    read: &str,
    write: &str,
    secrets: &[String],
) -> Result<u64, Failure> {
    let mut rows = from
        .copy_out_raw(read)
        .await
        .map_err(|error| database(&error, secrets))?;
    let mut sink = into
        .copy_in_raw(write)
        .await
        .map_err(|error| database(&error, secrets))?;
    while let Some(chunk) = rows.next().await {
        let chunk = chunk.map_err(|error| database(&error, secrets))?;
        sink.send(chunk)
            .await
            .map_err(|error| database(&error, secrets))?;
    }
    sink.finish()
        .await
        .map_err(|error| database(&error, secrets))
}

async fn counts(
    conn: &mut PgConnection,
    ids: &[Uuid],
    secrets: &[String],
) -> Result<Counts, Failure> {
    sqlx::query_as::<_, Counts>(COUNTS)
        .bind(ids)
        .fetch_one(&mut *conn)
        .await
        .map_err(|error| database(&error, secrets))
}

fn uuid_array(ids: &[Uuid]) -> String {
    let mut out = String::from("ARRAY[");
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push('\'');
        out.push_str(&id.to_string());
        out.push_str("'::uuid");
    }
    out.push_str("]::uuid[]");
    out
}

fn quote_ident(name: &str) -> Result<String, Failure> {
    if ident_ok(name) {
        Ok(format!("\"{name}\""))
    } else {
        Err(Failure::Copy(format!(
            "column {name} is not a plain identifier"
        )))
    }
}

fn principal_ok(principal: &str) -> bool {
    !principal.is_empty()
        && principal
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
}

fn ident_ok(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

const COLUMNS: &str = "\
SELECT a.attname::text
  FROM pg_attribute a
  JOIN pg_type t ON t.oid = a.atttypid
 WHERE a.attrelid = to_regclass($1)
   AND a.attnum > 0
   AND NOT a.attisdropped
   AND a.attgenerated = ''
   AND t.typname NOT IN ('vector', 'halfvec')
 ORDER BY a.attnum";

const COUNTS: &str = "\
SELECT
    (SELECT count(*) FROM rh_conversations WHERE id = ANY($1)) AS conversations,
    (SELECT count(*) FROM rh_messages WHERE conversation_id = ANY($1)) AS messages,
    (SELECT count(*) FROM rh_summaries WHERE conversation_id = ANY($1)) AS summaries,
    (SELECT count(*) FROM rh_topics) AS topics,
    (SELECT count(*) FROM rh_topic_aliases) AS aliases,
    (SELECT count(*) FROM rh_session_placements) AS placements";

pub(crate) async fn trim_after(app: &str, until: &str, secrets: &[String]) -> Result<(), Failure> {
    let cutoff = chrono::DateTime::parse_from_rfc3339(until).map_err(|error| Failure::Usage {
        detail: format!("--until: {error}"),
    })?;
    let pool = sqlx::PgPool::connect(app)
        .await
        .map_err(|error| crate::failure::database(&error, secrets))?;
    let statements = [
        "UPDATE rh_summaries SET parent_id = NULL WHERE parent_id IN (SELECT id FROM rh_summaries WHERE latest_at >= $1)",
        "DELETE FROM rh_summary_sources WHERE summary_id IN (SELECT id FROM rh_summaries WHERE latest_at >= $1) OR message_id IN (SELECT id FROM rh_messages WHERE created_at >= $1)",
        "DELETE FROM rh_summaries WHERE latest_at >= $1",
        "DELETE FROM rh_messages WHERE created_at >= $1",
    ];
    for statement in statements {
        let done = sqlx::query(statement).bind(cutoff).execute(&pool).await;
        if let Err(error) = done {
            pool.close().await;
            return Err(crate::failure::database(&error, secrets));
        }
    }
    let left: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM rh_messages), (SELECT count(*) FROM rh_summaries)",
    )
    .fetch_one(&pool)
    .await
    .map_err(|error| crate::failure::database(&error, secrets))?;
    crate::failure::note(&format!(
        "trimmed after {until}: {} messages, {} summaries left",
        left.0, left.1
    ));
    pool.close().await;
    Ok(())
}
