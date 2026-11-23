use std::collections::BTreeMap;
use std::collections::BTreeSet;

use memory_sqlite::SqliteError;
use memory_sqlite::SqliteStore;
use rusqlite::Connection;
use rusqlite::TransactionBehavior;
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use tokio_util::sync::CancellationToken;

use super::catalog::{Relation, Table};
use super::pg::PgColumn;
use super::values::{Kind, vector};
use super::{Batch, Counts, Row, Seeds, remap_topics};
use crate::Error;

fn query(source: rusqlite::Error) -> SqliteError {
    SqliteError::Query {
        operation: "cross-backend transfer",
        source,
    }
}

fn invalid(message: impl Into<String>) -> SqliteError {
    SqliteError::Invalid {
        message: message.into(),
    }
}

pub(super) async fn columns(
    store: &SqliteStore,
) -> Result<BTreeMap<String, BTreeSet<String>>, Error> {
    Ok(store
        .with_conn(|conn| {
            let mut statement = conn
                .prepare(
                    "SELECT m.name, p.name FROM sqlite_schema m
            JOIN pragma_table_xinfo(m.name) p WHERE m.type = 'table' AND p.hidden = 0",
                )
                .map_err(query)?;
            let mut rows = statement.query([]).map_err(query)?;
            let mut columns = BTreeMap::<String, BTreeSet<String>>::new();
            while let Some(row) = rows.next().map_err(query)? {
                columns
                    .entry(row.get(0).map_err(query)?)
                    .or_default()
                    .insert(row.get(1).map_err(query)?);
            }
            Ok(columns)
        })
        .await?)
}

pub(super) async fn snapshot(
    store: &SqliteStore,
    tables: Vec<Table>,
    owner: &str,
    shutdown: CancellationToken,
) -> Result<(Vec<Batch>, Seeds, super::Omissions), Error> {
    let owner = owner.to_owned();
    Ok(store
        .with_conn(move |conn| {
            let tx = conn.transaction().map_err(query)?;
            let mut batches = Vec::new();
            check_owner(&tx, &tables, &owner)?;
            for table in tables {
                cancelled(&shutdown)?;
                let rows = read(&tx, &table, &owner)?;
                batches.push(Batch { table, rows });
            }
            let unattributed: i64 = tx.query_row(
                "SELECT count(*) FROM rh_topic_tombstones WHERE owner_user_id IS NULL AND NOT locally_deleted",
                [], |row| row.get(0),
            ).map_err(query)?;
            Ok((batches, seeds(&tx)?, super::Omissions {
                unattributed_topic_tombstones: u64::try_from(unattributed)
                    .map_err(|source| invalid(source.to_string()))?,
                ..super::Omissions::default()
            }))
        })
        .await?)
}

fn read(conn: &Connection, table: &Table, owner: &str) -> Result<Vec<Row>, SqliteError> {
    let projection = table
        .columns
        .iter()
        .map(|c| field(table, c))
        .collect::<Vec<_>>()
        .join(", ");
    let filter = filter(table.relation);
    let sql = format!(
        "SELECT {projection} FROM {} r WHERE {filter}",
        table.relation.sqlite()
    );
    let mut statement = conn.prepare(&sql).map_err(query)?;
    let rows = statement
        .query_map([owner], |row| {
            table
                .columns
                .iter()
                .enumerate()
                .map(|(index, col)| {
                    if matches!(col.kind, Kind::Vector) && table.relation.stores_embedding() {
                        row.get::<_, Option<Vec<u8>>>(index)?
                            .as_deref()
                            .map(vector)
                            .transpose()
                    } else if matches!(col.kind, Kind::Single | Kind::Double) {
                        Ok(row
                            .get::<_, Option<f64>>(index)?
                            .map(|value| value.to_string()))
                    } else {
                        row.get(index)
                    }
                })
                .collect()
        })
        .map_err(query)?;
    let mut result = rows.collect::<Result<Vec<Row>, _>>().map_err(query)?;
    if table.relation == Relation::Conversations
        && let Some(visibility) = super::position(table, "visibility")
    {
        let session_key = super::position(table, "session_key");
        for row in &mut result {
            let key = super::cell(row, session_key)
                .ok_or_else(|| invalid("conversation has no session key"))?;
            let effective = SqliteStore::stored_session_visibility(conn, key, owner)?;
            if let Some(value) = row.get_mut(visibility) {
                *value = Some(effective);
            }
        }
    }
    Ok(result)
}

fn filter(relation: Relation) -> &'static str {
    if relation == Relation::Spaces {
        "r.id IN (SELECT space_id FROM rh_space_members WHERE user_id = ?1)"
    } else if relation == Relation::SpaceMembers {
        "r.user_id = ?1"
    } else if relation == Relation::SummaryPassages {
        "r.record_id IN (
            SELECT s.id FROM ros_summaries s LEFT JOIN ros_conversations c ON c.id = s.conversation_id
            WHERE coalesce(c.owner_user_id, ?1) = ?1
        )"
    } else if relation == Relation::Topics {
        "?1 IS NOT NULL AND (r.owner_user_id IS NOT NULL OR r.kind <> 'type')"
    } else if relation == Relation::TopicTombstones {
        "r.owner_user_id = ?1 OR r.locally_deleted"
    } else if relation == Relation::TagTaxonomy {
        "r.owner_user_id = ?1 OR (r.owner_user_id IS NULL AND (
            r.globally_shared IS NULL OR EXISTS (
                SELECT 1 FROM ros_tags t WHERE t.key = r.key AND t.value = r.value)))"
    } else {
        "?1 IS NOT NULL"
    }
}

fn field(table: &Table, column: &PgColumn) -> String {
    let name = column.name.as_str();
    let relation = table.relation;
    let value = match (relation, name) {
        (Relation::TagTaxonomy, "owner_user_id") => {
            "CASE WHEN r.globally_shared IS NULL THEN coalesce(r.owner_user_id, ?1) END".to_owned()
        }
        (_, "owner_user_id") if table.local.contains(name) => {
            "coalesce(r.owner_user_id, ?1)".to_owned()
        }
        (_, "owner_user_id") => "?1".to_owned(),
        (Relation::Conversations, "node") => {
            "(SELECT node FROM rh_conversation_nodes WHERE conversation_id = r.id)".to_owned()
        }
        (Relation::Conversations, "last_summary_input_at") => {
            "(SELECT captured_at FROM rh_summary_capture_clock WHERE conversation_id = r.id)"
                .to_owned()
        }
        (Relation::WikiTopics, "topic_id") => {
            "(SELECT topic_id FROM rh_topic_pages WHERE page_slug = r.slug)".to_owned()
        }
        (Relation::WikiTopics, "archived_sections") => {
            "(SELECT CASE WHEN sections_were_null AND sections = '[]' THEN NULL ELSE sections END
              FROM rh_topic_pages WHERE page_slug = r.slug)"
                .to_owned()
        }
        (_, "embedding") if !relation.stores_embedding() => "NULL".to_owned(),
        (Relation::Conversations, "visibility") | (_, "embed_error" | "embed_status") => {
            "NULL".to_owned()
        }
        (_, "embed_failures") => "0".to_owned(),
        (Relation::Summaries, "index_needs_repair") => {
            "(r.kind IN ('session', 'part') OR r.index_needs_repair)".to_owned()
        }
        (_, _) => format!("r.{name}"),
    };
    column.kind.sqlite_read(&value)
}

fn check_owner<'a>(
    conn: &Connection,
    tables: impl IntoIterator<Item = &'a Table>,
    owner: &str,
) -> Result<(), SqliteError> {
    let archived_owner: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('rh_topic_pages') WHERE name = 'owner_user_id')",
        [],
        |row| row.get(0),
    ).map_err(query)?;
    if archived_owner {
        verify_owner(conn, "rh_topic_pages", "owner_user_id", owner)?;
    }
    for table in tables {
        let name = table.relation.sqlite();
        let field = if table.relation == Relation::UserSettings {
            "user_id"
        } else if table.local.contains("owner_user_id") {
            "owner_user_id"
        } else {
            continue;
        };
        verify_owner(conn, &name, field, owner)?;
    }
    Ok(())
}

fn verify_owner(
    conn: &Connection,
    name: &str,
    field: &str,
    owner: &str,
) -> Result<(), SqliteError> {
    let sql = format!("SELECT EXISTS (SELECT 1 FROM {name} WHERE {field} <> ?1)");
    let other: bool = conn
        .query_row(&sql, [owner], |row| row.get(0))
        .map_err(query)?;
    if other {
        return Err(invalid(format!(
            "{name} contains another owner; --sqlite-owner or --owner must match the SQLite store"
        )));
    }
    Ok(())
}

pub(super) async fn load(
    store: &SqliteStore,
    mut batches: Vec<Batch>,
    source_seeds: Seeds,
    owner: &str,
    (dry_run, index_key): (bool, Option<[u8; 32]>),
    shutdown: CancellationToken,
) -> Result<Counts, Error> {
    let owner = owner.to_owned();
    Ok(store
        .with_conn(move |conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(query)?;
            tx.execute_batch("PRAGMA defer_foreign_keys = ON")
                .map_err(query)?;
            let mut added = Counts::new();
            check_owner(&tx, batches.iter().map(|batch| &batch.table), &owner)?;
            remap_topics(&mut batches, source_seeds, &seeds(&tx)?).map_err(invalid)?;
            for batch in &mut batches {
                cancelled(&shutdown)?;
                let count = insert(&tx, batch)?;
                if batch.table.relation == Relation::SummaryPassages
                    && !batch.rows.is_empty()
                    && let Some(key) = index_key
                {
                    super::passages::finish_sqlite(&tx, batch, &key)?;
                }
                added.insert(batch.table.relation.pg(), count);
            }
            cancelled(&shutdown)?;
            for batch in &batches {
                if batch.table.relation == Relation::Conversations {
                    verify_visibility(&tx, batch, &owner)?;
                }
            }
            if dry_run {
                let violation = tx
                    .query_row(
                        "SELECT EXISTS (SELECT 1 FROM pragma_foreign_key_check)",
                        [],
                        |r| r.get::<_, bool>(0),
                    )
                    .map_err(query)?;
                if violation {
                    return Err(invalid(
                        "SQLite destination would have foreign key violations",
                    ));
                }
                tx.rollback().map_err(query)?;
            } else {
                tx.commit().map_err(query)?;
            }
            Ok(added)
        })
        .await?)
}

fn seeds(conn: &Connection) -> Result<Seeds, SqliteError> {
    let mut statement = conn
        .prepare("SELECT slug, id FROM rh_topics WHERE kind = 'type' AND owner_user_id IS NULL")
        .map_err(query)?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(query)?;
    rows.collect::<Result<_, _>>().map_err(query)
}

fn insert(conn: &Connection, batch: &Batch) -> Result<u64, SqliteError> {
    if batch.rows.is_empty() {
        return Ok(0);
    }
    let table = &batch.table;
    let name = table.relation.sqlite();
    let cols = table
        .columns
        .iter()
        .enumerate()
        .filter(|(_, c)| table.local.contains(&c.name))
        .collect::<Vec<_>>();
    let projection = cols
        .iter()
        .map(|(_, c)| c.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let bindings = (1..=cols.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql =
        format!("INSERT INTO {name} ({projection}) VALUES ({bindings}) ON CONFLICT DO NOTHING");
    let mut statement = conn.prepare(&sql).map_err(query)?;
    let compared: Vec<_> = cols
        .iter()
        .enumerate()
        .filter_map(|(binding, (_, col))| {
            (col.primary || col.comparable).then_some((binding, *col))
        })
        .collect();
    let mut matching = conn.prepare(&check_sql(table, &compared)).map_err(query)?;
    let mut added = 0;
    let mut deleted = check_deletions(conn, table.relation)?;
    for row in &batch.rows {
        if let Some(statement) = &mut deleted {
            let key = super::cell(row, super::position(table, "session_key"));
            if statement
                .query_row([key], |r| r.get::<_, bool>(0))
                .map_err(query)?
            {
                return Err(invalid(
                    "session deletion conflict: a destination tombstone and incoming conversation, or vice versa; no rows committed",
                ));
            }
        }
        let values = cols
            .iter()
            .map(|(i, c)| c.kind.bind(super::cell(row, Some(*i))))
            .collect::<Result<Vec<Value>, _>>()
            .map_err(query)?;
        let count = statement
            .execute(params_from_iter(&values))
            .map_err(|source| invalid(format!("{name}: {source}")))?;
        if count == 0 {
            let parameters = compared.iter().filter_map(|(i, _)| values.get(*i));
            if !matching
                .query_row(params_from_iter(parameters), |r| r.get::<_, bool>(0))
                .map_err(query)?
            {
                return Err(conflict(&name));
            }
            if table.relation == Relation::SummaryPassages {
                super::passages::fill_sqlite(conn, table, row)?;
            }
        }
        apply_extra(conn, table, row)?;
        added += count as u64;
    }
    Ok(added)
}

fn check_sql(table: &Table, columns: &[(usize, &PgColumn)]) -> String {
    let name = table.relation.sqlite();
    let equal = columns
        .iter()
        .enumerate()
        .map(|(i, (_, c))| {
            if c.name == "owner_user_id" {
                if table.relation == Relation::TopicTombstones {
                    return format!("(locally_deleted OR owner_user_id IS ?{})", i + 1);
                }
                if table.relation == Relation::TagTaxonomy {
                    return format!(
                        "(owner_user_id IS ?{} AND (globally_shared IS NOT NULL) = (?{} IS NULL)
                           OR owner_user_id IS NULL AND globally_shared IS NULL AND ?{} IS NOT NULL)",
                        i + 1,
                        i + 1,
                        i + 1
                    );
                }
                return format!("(owner_user_id IS NULL OR owner_user_id IS ?{})", i + 1);
            }
            format!("({})", c.kind.compare(&c.name, &format!("?{}", i + 1)))
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    format!("SELECT EXISTS (SELECT 1 FROM {name} WHERE {equal})")
}

fn apply_extra(conn: &Connection, table: &Table, row: &Row) -> Result<(), SqliteError> {
    let find = |name| super::cell(row, super::position(table, name));
    if table.relation == Relation::WikiRedirects {
        conn.execute(
            "UPDATE ros_wiki_redirects SET owner_user_id = ?2 WHERE from_slug = ?1 AND owner_user_id IS NULL",
            rusqlite::params![find("from_slug"), find("owner_user_id")],
        ).map_err(query)?;
    }
    if table.relation == Relation::Conversations {
        extra(
            conn,
            "rh_conversation_nodes",
            "conversation_id",
            "node",
            find("id"),
            find("node"),
        )?;
        extra(
            conn,
            "rh_summary_capture_clock",
            "conversation_id",
            "captured_at",
            find("id"),
            find("last_summary_input_at"),
        )?;
    } else if table.relation == Relation::WikiTopics {
        apply_archive(
            conn,
            find("slug"),
            find("topic_id"),
            find("archived_sections"),
        )?;
    } else if table.relation == Relation::TagTaxonomy && find("owner_user_id").is_none() {
        conn.execute(
            "UPDATE ros_tag_taxonomy SET globally_shared = 1 WHERE key = ?1 AND value = ?2",
            rusqlite::params![find("key"), find("value")],
        )
        .map_err(query)?;
    }
    Ok(())
}

fn apply_archive(
    conn: &Connection,
    slug: Option<&str>,
    topic_id: Option<&str>,
    sections: Option<&str>,
) -> Result<(), SqliteError> {
    if topic_id.is_some() || sections.is_some() {
        conn.execute(
            "INSERT INTO rh_topic_pages (page_slug, topic_id, sections, sections_were_null)
             VALUES (?1, ?2, coalesce(?3, '[]'), ?3 IS NULL) ON CONFLICT DO NOTHING",
            rusqlite::params![slug, topic_id, sections],
        )
        .map_err(query)?;
    }
    let matches = conn
        .query_row(
            "SELECT EXISTS (
            SELECT 1 FROM rh_topic_pages WHERE page_slug = ?1 AND topic_id IS ?2
            AND (CASE WHEN sections_were_null AND sections = '[]' THEN NULL ELSE sections END) IS ?3
        ) OR (?2 IS NULL AND ?3 IS NULL AND NOT EXISTS (
            SELECT 1 FROM rh_topic_pages WHERE page_slug = ?1
                AND (topic_id IS NOT NULL OR NOT sections_were_null OR sections <> '[]')
        ))",
            rusqlite::params![slug, topic_id, sections],
            |r| r.get::<_, bool>(0),
        )
        .map_err(query)?;
    if !matches {
        return Err(conflict("rh_topic_pages"));
    }
    Ok(())
}

fn extra(
    conn: &Connection,
    table: &str,
    key: &str,
    field: &str,
    id: Option<&str>,
    value: Option<&str>,
) -> Result<(), SqliteError> {
    let comparison = if field == "captured_at" {
        "rh_instant(captured_at) IS rh_instant(?2)".to_owned()
    } else {
        format!("{field} IS ?2")
    };
    if value.is_some() {
        let sql =
            format!("INSERT INTO {table} ({key}, {field}) VALUES (?1, ?2) ON CONFLICT DO NOTHING");
        conn.execute(&sql, [id, value]).map_err(query)?;
    }
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM {table} WHERE {key} = ?1 AND ({comparison}))
       OR (?2 IS NULL AND NOT EXISTS (SELECT 1 FROM {table} WHERE {key} = ?1))"
    );
    if !conn
        .query_row(&sql, [id, value], |r| r.get::<_, bool>(0))
        .map_err(query)?
    {
        return Err(conflict(table));
    }
    Ok(())
}

fn conflict(table: &str) -> SqliteError {
    invalid(format!(
        "conflict in {table}: destination has different or newer data; no rows committed"
    ))
}

fn cancelled(token: &CancellationToken) -> Result<(), SqliteError> {
    if token.is_cancelled() {
        return Err(invalid("transfer cancelled"));
    }
    Ok(())
}

fn check_deletions(
    conn: &Connection,
    relation: Relation,
) -> Result<Option<rusqlite::Statement<'_>>, SqliteError> {
    let Some(table) = relation.deletion_counterpart().map(Relation::sqlite) else {
        return Ok(None);
    };
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM {table}
        WHERE session_key = ?1 OR session_key = 'claude-code:' || ?1
        OR ?1 = 'claude-code:' || session_key)"
    );
    conn.prepare(&sql).map(Some).map_err(query)
}

fn verify_visibility(conn: &Connection, batch: &Batch, owner: &str) -> Result<(), SqliteError> {
    let session = super::position(&batch.table, "session_key");
    let visibility = super::position(&batch.table, "visibility");
    for row in &batch.rows {
        let key =
            super::cell(row, session).ok_or_else(|| invalid("conversation has no session key"))?;
        let effective = SqliteStore::stored_session_visibility(conn, key, owner)?;
        if Some(effective.as_str()) != super::cell(row, visibility) {
            return Err(invalid(
                "session visibility differs in SQLite; no rows committed",
            ));
        }
    }
    Ok(())
}
