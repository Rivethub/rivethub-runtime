use std::collections::BTreeMap;

use memory_sqlite::{SqliteError, SqliteStore};
use rusqlite::Connection;
use sqlx::{AssertSqlSafe, PgPool};
use strum::IntoEnumIterator as _;

use super::catalog::Relation;
use super::values::Kind;
use super::{Direction, Seeds, pg, plan};

type StoreRows = BTreeMap<String, Vec<String>>;
type TestError = Box<dyn std::error::Error>;

pub(super) async fn postgres(pool: &PgPool) -> Result<StoreRows, TestError> {
    let mut conn = pg::begin(pool, Direction::ToSqlite).await?;
    let seeds = pg::seeds(&mut conn).await?;
    let mut rows = StoreRows::new();
    for relation in Relation::iter() {
        let table = relation.pg();
        let filter = if relation == Relation::Topics {
            "WHERE owner_user_id IS NOT NULL OR kind <> 'type'"
        } else {
            ""
        };
        let sql = format!("SELECT to_jsonb(r)::text FROM {table} r {filter}");
        let contents = sqlx::query_scalar(AssertSqlSafe(sql))
            .fetch_all(&mut *conn)
            .await?;
        rows.insert(table, normalize_seeds(contents, &seeds));
    }
    Ok(rows)
}

pub(super) async fn sqlite(pool: &PgPool, store: &SqliteStore) -> Result<StoreRows, TestError> {
    let mut conn = pool.acquire().await?;
    let tables = plan(&mut conn, store).await?;
    Ok(store
        .with_conn(move |conn| {
            conn.create_scalar_function(
                "rh_transfer_halfvec",
                1,
                rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
                |ctx| {
                    let bytes = ctx.get::<Option<Vec<u8>>>(0)?;
                    Ok(bytes.map(|bytes| {
                        let (floats, _) = bytes.as_chunks::<4>();
                        let half: Vec<_> = floats
                            .iter()
                            .copied()
                            .map(f32::from_le_bytes)
                            .map(half::f16::from_f32)
                            .flat_map(half::f16::to_le_bytes)
                            .collect();
                        hex::encode(half)
                    }))
                },
            )
            .map_err(SqliteError::Pragma)?;
            let seeds = read_seeds(conn)?;
            let mut rows = StoreRows::new();
            for table in tables {
                let name = table.relation.sqlite();
                let filter = if table.relation == Relation::Topics {
                    "WHERE owner_user_id IS NOT NULL OR kind <> 'type'"
                } else {
                    ""
                };
                let fields = table
                    .columns
                    .into_iter()
                    .map(|column| (column.name, column.kind))
                    .collect();
                rows.insert(
                    name.clone(),
                    read_rows(conn, &name, &fields, filter, &seeds)?,
                );
            }
            for name in [
                "rh_conversation_nodes",
                "rh_summary_capture_clock",
                "rh_topic_pages",
            ] {
                rows.insert(
                    name.to_owned(),
                    read_rows(conn, name, &BTreeMap::new(), "", &seeds)?,
                );
            }
            Ok(rows)
        })
        .await?)
}

fn read_seeds(conn: &Connection) -> Result<Seeds, SqliteError> {
    conn.prepare("SELECT slug, id FROM rh_topics WHERE kind = 'type' AND owner_user_id IS NULL")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect()
        })
        .map_err(SqliteError::Pragma)
}

fn read_rows(
    conn: &Connection,
    table: &str,
    fields: &BTreeMap<String, Kind>,
    filter: &str,
    seeds: &Seeds,
) -> Result<Vec<String>, SqliteError> {
    let mut columns = conn
        .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY name")
        .map_err(SqliteError::Pragma)?;
    let fields = columns
        .query_map([table], |row| row.get::<_, String>(0))
        .map_err(SqliteError::Pragma)?
        .filter_map(|name| match name {
            Ok(name) if table == "ros_summary_passages" && name == "fts_rowid" => None,
            Ok(name) => Some(Ok(projection(&name, fields.get(&name).copied()))),
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::Pragma)?
        .join(", ");
    let sql = format!("SELECT json_array({fields}) FROM {table} r {filter}");
    let rows = conn
        .prepare(&sql)
        .and_then(|mut statement| statement.query_map([], |row| row.get(0))?.collect())
        .map_err(SqliteError::Pragma)?;
    Ok(normalize_seeds(rows, seeds))
}

fn projection(name: &str, kind: Option<Kind>) -> String {
    if matches!(kind, Some(Kind::Vector)) {
        format!("rh_transfer_halfvec(r.{name})")
    } else if matches!(kind, Some(Kind::Json | Kind::TextArray)) {
        format!(
            "CASE WHEN r.{name} IS NOT NULL THEN (
            SELECT json_group_array(json_array(fullkey, type, atom))
            FROM (SELECT fullkey, type, atom FROM json_tree(r.{name}) ORDER BY fullkey)
        ) END"
        )
    } else if matches!(kind, Some(Kind::Instant)) || name == "captured_at" {
        format!("rh_instant(r.{name})")
    } else {
        format!("CASE WHEN typeof(r.{name}) = 'blob' THEN hex(r.{name}) ELSE r.{name} END")
    }
}

fn normalize_seeds(mut rows: Vec<String>, seeds: &Seeds) -> Vec<String> {
    for row in &mut rows {
        for (slug, id) in seeds {
            *row = row.replace(&format!("\"{id}\""), &format!("\"type:{slug}\""));
        }
    }
    rows.sort_unstable();
    rows
}

pub(super) fn assert_same(expected: &StoreRows, actual: &StoreRows) {
    assert_eq!(
        expected.keys().collect::<Vec<_>>(),
        actual.keys().collect::<Vec<_>>()
    );
    for (table, rows) in expected {
        assert_eq!(Some(rows), actual.get(table), "{table}");
    }
}
