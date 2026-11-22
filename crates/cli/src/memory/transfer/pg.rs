use std::collections::BTreeSet;

use sqlx::AssertSqlSafe;
use sqlx::PgConnection;
use sqlx::PgPool;
use sqlx::Postgres;
use sqlx::Transaction;
use tokio_util::sync::CancellationToken;

use super::Batch;
use super::Counts;
use super::Direction;
use super::Row;
use super::Seeds;
use super::catalog::Relation;
use super::catalog::Table;
use super::values::Kind;
use super::{cancelled, counts, identifier, invalid, remap_topics};
use crate::Error;

#[derive(Debug)]
pub(super) struct PgColumn {
    pub name: String,
    pub cast: String,
    pub kind: Kind,
    pub primary: bool,
}

pub(super) fn error(source: sqlx::Error) -> Error {
    Error::TransferDatabase(source)
}

pub(super) async fn begin(
    pool: &PgPool,
    direction: Direction,
) -> Result<Transaction<'static, Postgres>, Error> {
    let mut tx = pool.begin().await.map_err(error)?;
    let mode = match direction {
        Direction::ToPostgres => "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
        Direction::ToSqlite => "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
    };
    sqlx::raw_sql(mode).execute(&mut *tx).await.map_err(error)?;
    if direction == Direction::ToPostgres {
        sqlx::raw_sql("SET CONSTRAINTS ALL DEFERRED")
            .execute(&mut *tx)
            .await
            .map_err(error)?;
    }
    sqlx::query!("SELECT set_config('rh.maintenance', 'on', true)")
        .fetch_one(&mut *tx)
        .await
        .map_err(error)?;
    let pin = sqlx::query_scalar!("SELECT rh_scope_pin() AS pin")
        .fetch_one(&mut *tx)
        .await
        .map_err(error)?;
    if pin.is_some() {
        return Err(invalid(
            "use the hub's administrative database connection for cross-backend transfer",
        ));
    }
    Ok(tx)
}

pub(super) async fn columns(
    conn: &mut PgConnection,
    relation: Relation,
    local: &BTreeSet<String>,
) -> Result<Vec<PgColumn>, Error> {
    let table = format!("public.{}", relation.pg());
    let columns = sqlx::query!(
        "SELECT a.attname::text AS \"name!\", t.typname::text AS \"type_name!\",
                EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid
                        AND i.indisprimary AND a.attnum = ANY(i.indkey)) AS \"primary!\"
           FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid
          WHERE a.attrelid = to_regclass($1) AND a.attnum > 0
            AND NOT a.attisdropped AND a.attgenerated = ''
          ORDER BY a.attnum",
        table
    )
    .fetch_all(conn)
    .await
    .map_err(error)?;
    columns
        .into_iter()
        .filter(|column| {
            local.contains(&column.name)
                || column.name == "owner_user_id"
                || relation.sqlite_extra(&column.name)
        })
        .map(|column| {
            identifier(&column.name)?;
            let kind = Kind::parse(&column.type_name)?;
            Ok(PgColumn {
                name: column.name,
                cast: column.type_name,
                kind,
                primary: column.primary,
            })
        })
        .collect()
}

pub(super) async fn snapshot(
    conn: &mut PgConnection,
    tables: Vec<Table>,
    owner: &str,
    shutdown: &CancellationToken,
) -> Result<Vec<Batch>, Error> {
    let mut snapshot = Vec::new();
    for table in tables {
        cancelled(shutdown)?;
        let fields = table
            .columns
            .iter()
            .map(|c| projection(table.relation, c))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT ARRAY[{fields}] FROM public.{} r WHERE {}",
            table.relation.pg(),
            table.relation.owner_predicate()?
        );
        let rows = sqlx::query_scalar::<_, Row>(AssertSqlSafe(sql))
            .bind(owner)
            .fetch_all(&mut *conn)
            .await
            .map_err(error)?;
        snapshot.push(Batch { table, rows });
    }
    Ok(snapshot)
}

pub(super) async fn omissions(
    conn: &mut PgConnection,
    owner: &str,
    batches: &[Batch],
) -> Result<super::Omissions, Error> {
    let pages = batches
        .iter()
        .find(|batch| batch.table.relation == Relation::WikiTopics)
        .ok_or_else(|| invalid("missing wiki pages from transfer snapshot"))?;
    let slug = super::position(&pages.table, "slug");
    let slugs: Vec<_> = pages
        .rows
        .iter()
        .filter_map(|row| super::cell(row, slug).map(str::to_owned))
        .collect();
    let claims_on_other_topics = excluded_claims(conn, owner).await?;
    let redirects_owned_by_others = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM rh_wiki_redirects r
         WHERE r.owner_user_id <> $1 AND r.to_slug = ANY($2)"#,
        owner,
        &slugs
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(error)?;
    let unattributed_topic_tombstones = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM rh_topic_tombstones WHERE owner_user_id IS NULL"#
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(error)?;
    Ok(super::Omissions {
        claims_on_other_topics,
        redirects_owned_by_others: omission_count(redirects_owned_by_others)?,
        unattributed_topic_tombstones: omission_count(unattributed_topic_tombstones)?,
    })
}

fn omission_count(count: i64) -> Result<u64, Error> {
    u64::try_from(count).map_err(|err| invalid(err.to_string()))
}

async fn excluded_claims(conn: &mut PgConnection, owner: &str) -> Result<u64, Error> {
    let predicate = Relation::EntityFactClaims.owner_predicate()?;
    let sql = format!(
        "SELECT count(*) FROM rh_entity_fact_claims r WHERE r.owner_user_id = $1 AND NOT ({predicate})"
    );
    let count: i64 = sqlx::query_scalar(AssertSqlSafe(sql))
        .bind(owner)
        .fetch_one(conn)
        .await
        .map_err(error)?;
    omission_count(count)
}

fn projection(relation: Relation, column: &PgColumn) -> String {
    let name = &column.name;
    match (relation, name.as_str()) {
        (Relation::Messages | Relation::Summaries, "access_count" | "last_accessed_at") => {
            let (table, key) = if relation == Relation::Messages {
        (Relation::WikiRedirects, "owner_user_id") => "coalesce(r.owner_user_id, $1)".to_owned(),
                ("rh_message_access", "message_id")
            } else {
                ("rh_summary_access", "summary_id")
            };
            let value = format!(
                "coalesce((SELECT a.{name} FROM {table} a WHERE a.{key} = r.id), r.{name})"
            );
            column.kind.project(&value)
        }
        (Relation::Messages, "content") => "coalesce(r.content, '')".to_owned(),
        (Relation::Messages, "metadata") => "coalesce(r.metadata, '{}'::jsonb)::text".to_owned(),
        (Relation::Conversations, "settings") => {
            "coalesce(r.settings, '{}'::jsonb)::text".to_owned()
        }
        (_, "embedding") if !relation.stores_embedding() => "NULL::text".to_owned(),
        (_, "embed_status" | "embed_error") => "NULL::text".to_owned(),
        (_, "embed_failures") => "'0'::text".to_owned(),
        (Relation::Summaries, "index_needs_repair") => {
            "(r.kind IN ('session', 'part') OR r.index_needs_repair)::text".to_owned()
        }
        _ => column.kind.project(&format!("r.{name}")),
    }
}

pub(super) async fn load(
    conn: &mut PgConnection,
    batches: &mut [Batch],
    source_seeds: Seeds,
    owner: &str,
    index_key: Option<[u8; 32]>,
    shutdown: &CancellationToken,
) -> Result<(Counts, Counts), Error> {
    let read = counts(batches);
    let mut added = Counts::new();
    remap_topics(batches, source_seeds, &seeds(conn).await?).map_err(invalid)?;
    for batch in &mut *batches {
        cancelled(shutdown)?;
        let inserted = if batch.rows.is_empty() {
            0
        } else {
            insert(conn, batch).await?
        };
        if batch.table.relation == Relation::SummaryPassages
            && inserted > 0
            && let Some(key) = index_key
        {
            super::passages::finish_postgres(conn, batch, &key).await?;
        }
        added.insert(batch.table.relation.pg(), inserted);
    }
    sqlx::query!("SELECT rh_refresh_owner($1)", owner)
        .execute(&mut *conn)
        .await
        .map_err(error)?;
    for batch in batches {
        if !batch.rows.is_empty()
            && matches!(
                batch.table.relation,
                Relation::Topics | Relation::Conversations
            )
        {
            verify_visibility(conn, batch.table.relation).await?;
        }
    }
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *conn)
        .await
        .map_err(error)?;
    Ok((read, added))
}

pub(super) async fn seeds(conn: &mut PgConnection) -> Result<Seeds, Error> {
    let rows = sqlx::query!("SELECT slug, id::text AS \"id!\" FROM rh_topics WHERE kind = 'type' AND owner_user_id IS NULL")
        .fetch_all(conn)
        .await
        .map_err(error)?;
    Ok(rows.into_iter().map(|r| (r.slug, r.id)).collect())
}

async fn stage(conn: &mut PgConnection, batch: &Batch, stage: &str) -> Result<(), Error> {
    let names = batch.table.names().join(", ");
    let (parameters, projections): (Vec<_>, Vec<_>) = batch
        .table
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let parameter = format!("${}::text[]", i + 1);
            let value = c.kind.cast(&format!("incoming.{}", c.name), &c.cast);
            let value = if batch.table.relation == Relation::EntityEmbeddings
                && c.name == "sqlite_embedding"
            {
                format!(
                    "CASE WHEN incoming.embedding::vector
                    IS DISTINCT FROM incoming.embedding::halfvec::vector THEN {value} END"
                )
            } else {
                value
            };
            (parameter, format!("{value} AS {}", c.name))
        })
        .unzip();
    let sql = format!(
        "CREATE TEMP TABLE {stage} ON COMMIT DROP AS
        SELECT {} FROM unnest({}) AS incoming ({names})",
        projections.join(", "),
        parameters.join(", ")
    );
    let mut query = sqlx::query(AssertSqlSafe(sql));
    for (index, _) in batch.table.columns.iter().enumerate() {
        let data: Vec<_> = batch
            .rows
            .iter()
            .map(|row| row.get(index).and_then(Option::as_deref))
            .collect();
        query = query.bind(data);
    }
    query.execute(conn).await.map_err(error)?;
    Ok(())
}

async fn insert(conn: &mut PgConnection, batch: &Batch) -> Result<u64, Error> {
    let table = batch.table.relation.pg();
    let stage_name = format!("transfer_{table}");
    stage(conn, batch, &stage_name).await?;
    let names = batch.table.names().join(", ");
    if batch.table.relation.authorization() {
        check_existing(conn, batch, &stage_name)
            .await
            .map_err(|error| {
                invalid(format!(
                    "hub space and membership must already match the SQLite source: {error}"
                ))
            })?;
        return Ok(0);
    }
    check_deletions(conn, batch.table.relation, &stage_name).await?;
    let sql = format!(
        "INSERT INTO public.{table} ({names}) SELECT {names} FROM {stage_name} ON CONFLICT DO NOTHING"
    );
    let added = sqlx::query(AssertSqlSafe(sql))
        .execute(&mut *conn)
        .await
        .map_err(|source| invalid(format!("{table}: {source}")))?
        .rows_affected();
    if added < batch.rows.len() as u64 {
        check_existing(conn, batch, &stage_name).await?;
    }
    Ok(added)
}

async fn check_existing(conn: &mut PgConnection, batch: &Batch, stage: &str) -> Result<(), Error> {
    let table = batch.table.relation.pg();
    let key = batch
        .table
        .primary_key()
        .into_iter()
        .map(|name| format!("s.{name} = r.{name}"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let equal = batch.table.columns.iter()
        .filter(|c| comparable(batch.table.relation, &c.name))
        .map(|c| {
            let name = &c.name;
            let value = match (batch.table.relation, name.as_str()) {
                (Relation::Messages, "access_count" | "last_accessed_at") =>
                    format!("coalesce((SELECT {name} FROM rh_message_access WHERE message_id = r.id), r.{name})"),
                (Relation::Summaries, "access_count" | "last_accessed_at") =>
                    format!("coalesce((SELECT {name} FROM rh_summary_access WHERE summary_id = r.id), r.{name})"),
                _ => format!("r.{name}"),
            };
            if batch.table.relation == Relation::WikiRedirects && name == "owner_user_id" {
                format!("({value} IS NULL OR s.{name} = {value})")
            } else {
                format!("s.{name} IS NOT DISTINCT FROM {value}")
            }
        })
        .collect::<Vec<_>>().join(" AND ");
    let query = format!(
        "SELECT EXISTS (SELECT 1 FROM {stage} s WHERE NOT EXISTS
        (SELECT 1 FROM public.{table} r WHERE {key} AND {equal}))"
    );
    let differs = sqlx::query_scalar::<_, bool>(AssertSqlSafe(query))
        .fetch_one(conn)
        .await
        .map_err(error)?;
    if differs {
        return Err(invalid(format!(
            "conflict in {table}: destination has different or newer data; no rows committed"
        )));
    }
    Ok(())
}

pub(super) fn comparable(relation: Relation, name: &str) -> bool {
    !matches!(
        name,
        "embed_status" | "embed_error" | "embed_failures" | "index_needs_repair"
    ) && (name != "embedding" || relation.stores_embedding())
        && !matches!(
            (relation, name),
            (Relation::Topics | Relation::Conversations, "visibility")
                | (Relation::SummaryPassages, "embedding")
        )
}

async fn check_deletions(
    conn: &mut PgConnection,
    relation: Relation,
    stage: &str,
) -> Result<(), Error> {
    let Some(against) = relation.deletion_counterpart().map(Relation::pg) else {
        return Ok(());
    };
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM {stage} s JOIN {against} r
        ON r.session_key = s.session_key OR r.session_key = 'claude-code:' || s.session_key
        OR s.session_key = 'claude-code:' || r.session_key)"
    );
    if sqlx::query_scalar::<_, bool>(AssertSqlSafe(sql))
        .fetch_one(conn)
        .await
        .map_err(error)?
    {
        return Err(invalid(
            "session deletion conflict: a destination tombstone and incoming conversation, or vice versa; no rows committed",
        ));
    }
    Ok(())
}

async fn verify_visibility(conn: &mut PgConnection, relation: Relation) -> Result<(), Error> {
    let table = relation.pg();
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM transfer_{table} s JOIN public.{table} r USING (id)
        WHERE r.visibility IS DISTINCT FROM s.visibility)"
    );
    if sqlx::query_scalar::<_, bool>(AssertSqlSafe(sql))
        .fetch_one(conn)
        .await
        .map_err(error)?
    {
        return Err(invalid(format!(
            "visibility differs in {table}; no rows committed"
        )));
    }
    Ok(())
}
