use jiff::Timestamp;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use rusqlite::Connection;
use rusqlite::types::ValueRef;
use serde::Deserialize;
use serde::de::DeserializeOwned;

pub(crate) type TestError = Box<dyn std::error::Error>;
pub(crate) type TestResult<T> = Result<T, TestError>;

pub(crate) fn recording(name: &str) -> PathBuf {
    testkit::paths::recording(&format!("memory-sqlite/{name}"))
}

pub(crate) fn parse<T: DeserializeOwned>(name: &str) -> TestResult<T> {
    let text =
        fs::read_to_string(recording(name)).map_err(|error| format!("read {name}: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {name}: {error}").into())
}

pub(crate) fn schema_recording() -> TestResult<SchemaRecording> {
    parse("schema.json")
}

pub(crate) fn upgrade_recording() -> TestResult<UpgradeRecording> {
    parse("upgrade.json")
}

pub(crate) fn fixture_copy(name: &str) -> TestResult<testkit::FixtureCopy> {
    Ok(testkit::copy_fixture(&recording(name))?)
}

pub(crate) fn instant(text: &str) -> TestResult<Timestamp> {
    Ok(text
        .parse::<Timestamp>()
        .map_err(|error| format!("parse instant {text}: {error}"))?)
}

pub(crate) fn connection(path: &Path) -> TestResult<Connection> {
    let conn =
        Connection::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|error| format!("enable foreign keys: {error}"))?;
    Ok(conn)
}

pub(crate) fn read_pragmas(conn: &Connection) -> TestResult<Pragmas> {
    Ok(Pragmas {
        user_version: scalar(conn, "PRAGMA user_version")?,
        journal_mode: text(conn, "PRAGMA journal_mode")?,
        foreign_keys: scalar(conn, "PRAGMA foreign_keys")?,
        page_size: scalar(conn, "PRAGMA page_size")?,
        encoding: text(conn, "PRAGMA encoding")?,
    })
}

pub(crate) fn read_legacy_master_entries(conn: &Connection) -> TestResult<Vec<MasterEntry>> {
    let mut statement =
        conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_master WHERE name NOT GLOB 'rh_*' AND tbl_name NOT GLOB 'rh_*' AND tbl_name <> 'ros_summary_deliveries'          AND name NOT GLOB 'ros_summary_passages*' AND tbl_name NOT GLOB 'ros_summary_passages*'          AND name <> 'ros_summary_records_indexed' AND name <> 'sqlite_sequence' ORDER BY rowid")?;
    let mut rows = statement.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        let mut sql: Option<String> = row.get(3)?;
        if let Some(sql) = &mut sql {
            if name == "ros_summaries"
                && let Some(start) = sql.find(", summary_ordinal INTEGER")
                && let Some(end) = sql.rfind(')')
            {
                sql.replace_range(start..end, "");
            }
            if name == "ros_conversations"
                && let Some(start) = sql.find(", title_from_capture INTEGER")
                && let Some(end) = sql.rfind(')')
            {
                sql.replace_range(start..end, "");
            }
            if name == "ros_wiki_topics"
                && let Some(start) = sql
                    .find(", visibility TEXT")
                    .or_else(|| sql.find(", history TEXT"))
                && let Some(end) = sql.rfind(')')
            {
                sql.replace_range(start..end, "");
            }
            if matches!(name.as_str(), "ros_tag_taxonomy" | "ros_wiki_redirects") {
                *sql = sql
                    .replace(", owner_user_id TEXT", "")
                    .replace(", globally_shared INTEGER CHECK (globally_shared = 1)", "");
            }
            if name == "ros_summary_sources" {
                *sql = sql.replace(", render_key BLOB CHECK (length(render_key) = 32)", "");
            }
        }
        out.push(MasterEntry {
            kind: row.get(0)?,
            name,
            tbl_name: row.get(2)?,
            sql,
        });
    }
    Ok(out)
}

pub(crate) fn read_table_columns(conn: &Connection, table: &str) -> TestResult<Vec<TableColumn>> {
    let mut statement = conn.prepare(
        "SELECT cid, name, type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1)",
    )?;
    let mut rows = statement.query([table])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if !legacy_column(table, &name) {
            continue;
        }
        out.push(TableColumn {
            cid: row.get(0)?,
            name,
            kind: row.get(2)?,
            notnull: row.get(3)?,
            dflt_value: row.get(4)?,
            pk: row.get(5)?,
        });
    }
    Ok(out)
}

pub(crate) fn read_rows(conn: &Connection, table: &str) -> TestResult<Vec<BTreeMap<String, Cell>>> {
    let mut statement = conn.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
    let names: Vec<String> = statement.column_names().map(str::to_string).collect();
    let mut rows = statement.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut cells = BTreeMap::new();
        for (index, name) in names.iter().enumerate() {
            if legacy_column(table, name) {
                cells.insert(name.clone(), cell(row.get_ref(index)?));
            }
        }
        out.push(cells);
    }
    Ok(out)
}

fn legacy_column(table: &str, column: &str) -> bool {
    !matches!(
        (table, column),
        ("ros_summary_sources", "render_key")
            | ("ros_conversations", "title_from_capture")
            | (
                "ros_wiki_topics",
                "visibility" | "scope" | "local_slug" | "history"
            )
            | ("ros_tag_taxonomy", "owner_user_id" | "globally_shared")
            | ("ros_wiki_redirects", "owner_user_id")
            | (
                "ros_summaries",
                "summary_ordinal"
                    | "index_needs_repair"
                    | "issued_part_ordinal"
                    | "input_token_estimate"
                    | "source_first"
                    | "source_last"
                    | "range_verified"
                    | "input_key"
                    | "index_key"
                    | "quality_flags"
                    | "section_char_start"
                    | "section_char_end"
            )
    )
}

pub(crate) fn read_fts_rows(
    conn: &Connection,
    table: &str,
) -> TestResult<Vec<BTreeMap<String, Cell>>> {
    let mut statement = conn.prepare(&format!("SELECT rowid, * FROM {table} ORDER BY rowid"))?;
    let names: Vec<String> = statement.column_names().map(str::to_string).collect();
    let mut rows = statement.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut cells = BTreeMap::new();
        for (index, name) in names.iter().enumerate() {
            cells.insert(name.clone(), cell(row.get_ref(index)?));
        }
        out.push(cells);
    }
    Ok(out)
}

pub(crate) fn read_row_count(conn: &Connection, table: &str) -> TestResult<i64> {
    Ok(
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })?,
    )
}

pub(crate) fn read_column_types(
    conn: &Connection,
    table: &str,
) -> TestResult<BTreeMap<String, BTreeMap<String, i64>>> {
    let columns = read_table_columns(conn, table)?;
    let mut out = BTreeMap::new();
    for column in columns {
        let mut statement = conn.prepare(&format!(
            "SELECT typeof(\"{}\") AS cell_kind, count(*) AS n FROM {table} GROUP BY 1 ORDER BY 1",
            column.name
        ))?;
        let mut rows = statement.query([])?;
        let mut counts = BTreeMap::new();
        while let Some(row) = rows.next()? {
            counts.insert(row.get::<_, String>(0)?, row.get::<_, i64>(1)?);
        }
        out.insert(column.name, counts);
    }
    Ok(out)
}

pub(crate) fn replay_search_access(
    conn: &Connection,
    snapshot: &MigratedSnapshot,
) -> TestResult<()> {
    for hit in &snapshot.search_after.hits {
        let table = match hit.layer.as_str() {
            "message" => "ros_messages",
            "summary" => "ros_summaries",
            other => return Err(format!("unexpected search layer {other}").into()),
        };
        let bumped = conn
            .execute(
                &format!(
                    "UPDATE {table} SET access_count = access_count + 1, last_accessed_at = ?1 WHERE id = ?2"
                ),
                rusqlite::params![snapshot.clock, hit.id],
            )
            .map_err(|error| format!("replay the access bump for {}: {error}", hit.id))?;
        assert_eq!(bumped, 1, "the recorded hit {} must exist", hit.id);
    }
    Ok(())
}

fn scalar(conn: &Connection, pragma: &str) -> TestResult<i64> {
    Ok(conn.query_row(pragma, [], |row| row.get(0))?)
}

fn text(conn: &Connection, pragma: &str) -> TestResult<String> {
    Ok(conn.query_row(pragma, [], |row| row.get(0))?)
}

fn cell(value: ValueRef<'_>) -> Cell {
    match value {
        ValueRef::Null => Cell::Null,
        ValueRef::Integer(found) => Cell::Integer(found),
        ValueRef::Real(found) => Cell::Real(found),
        ValueRef::Text(found) => Cell::Text(String::from_utf8_lossy(found).into_owned()),
        ValueRef::Blob(found) => Cell::Blob(BlobHex {
            hex: hex::encode(found),
        }),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SchemaRecording {
    pub(crate) fresh: SchemaSnapshot,
    #[serde(rename = "populated")]
    pub(crate) fixture: FixtureSnapshot,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SchemaSnapshot {
    pub(crate) pragmas: Pragmas,
    pub(crate) sqlite_master: Vec<MasterEntry>,
    pub(crate) table_info: BTreeMap<String, Vec<TableColumn>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FixtureSnapshot {
    pub(crate) pragmas: Pragmas,
    pub(crate) row_counts: BTreeMap<String, i64>,
    #[serde(rename = "typeof")]
    pub(crate) column_types: BTreeMap<String, BTreeMap<String, BTreeMap<String, i64>>>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct Pragmas {
    pub(crate) user_version: i64,
    pub(crate) journal_mode: String,
    pub(crate) foreign_keys: i64,
    pub(crate) page_size: i64,
    pub(crate) encoding: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct MasterEntry {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) tbl_name: String,
    pub(crate) sql: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct TableColumn {
    pub(crate) cid: i64,
    pub(crate) name: String,
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) notnull: i64,
    pub(crate) dflt_value: Option<String>,
    pub(crate) pk: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpgradeRecording {
    pub(crate) cases: Vec<UpgradeCase>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpgradeCase {
    pub(crate) input_file: String,
    pub(crate) migrated: MigratedSnapshot,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MigratedSnapshot {
    pub(crate) clock: String,
    pub(crate) pragmas: Pragmas,
    pub(crate) sqlite_master: Vec<MasterEntry>,
    pub(crate) table_info: BTreeMap<String, Vec<TableColumn>>,
    pub(crate) tables: BTreeMap<String, Vec<BTreeMap<String, Cell>>>,
    pub(crate) fts: BTreeMap<String, Vec<BTreeMap<String, Cell>>>,
    #[serde(rename = "typeof")]
    pub(crate) column_types: BTreeMap<String, BTreeMap<String, BTreeMap<String, i64>>>,
    pub(crate) search_after: SearchAfter,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SearchAfter {
    pub(crate) hits: Vec<SearchHit>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SearchHit {
    pub(crate) id: String,
    pub(crate) layer: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(BlobHex),
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct BlobHex {
    #[serde(rename = "$blob_hex")]
    pub(crate) hex: String,
}

impl PartialEq for Cell {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Integer(left), Self::Integer(right)) => left == right,
            (Self::Real(left), Self::Real(right)) => left == right,
            (Self::Integer(left), Self::Real(right)) | (Self::Real(right), Self::Integer(left)) => {
                #[expect(clippy::cast_precision_loss)]
                let widened = *left as f64;
                widened == *right
            }
            (Self::Text(left), Self::Text(right)) => left == right,
            (Self::Blob(left), Self::Blob(right)) => left == right,
            _ => false,
        }
    }
}

pub(crate) fn assert_upgraded_pragmas(actual: &Pragmas, legacy: &Pragmas) {
    assert_eq!(actual.user_version, memory_sqlite::SCHEMA_VERSION);
    assert_eq!(actual.journal_mode, legacy.journal_mode);
    assert_eq!(actual.foreign_keys, legacy.foreign_keys);
    assert_eq!(actual.page_size, legacy.page_size);
    assert_eq!(actual.encoding, legacy.encoding);
}
