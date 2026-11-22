mod catalog;
mod columns;
mod copy;
mod error;
mod sql;
mod values;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;
use tokio_util::sync::CancellationToken;

use crate::SqliteStore;

pub use error::ImportError;

const RECEIPT_KEY: &str = "pglite_import_report";

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Deserialize,
    Serialize,
    strum::EnumIter,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ImportTable {
    Conversations,
    Messages,
    Summaries,
    SummarySources,
    SummaryDeliveries,
    Tags,
    TagTaxonomy,
    WikiTopics,
    WikiProvenance,
    WikiExtractions,
    WikiCitations,
    WikiRedirects,
    Jobs,
    Meta,
    EmbedQueue,
}

impl ImportTable {
    pub fn pg_dump_arguments() -> Vec<String> {
        let mut arguments = vec![
            "--data-only".into(),
            "--format=plain".into(),
            "--no-owner".into(),
            "--no-privileges".into(),
        ];
        arguments.extend(
            Self::iter()
                .filter(|table| !matches!(table, Self::Jobs | Self::Meta | Self::EmbedQueue))
                .map(|table| format!("--table=public.{}", table.compatibility_name())),
        );
        arguments
    }

    pub fn compatibility_name(self) -> String {
        format!("ros_{self}")
    }

    #[must_use]
    pub fn dump_columns(self) -> Option<&'static [&'static str]> {
        catalog::columns(self)
    }

    #[must_use]
    pub fn dump_order(self) -> &'static str {
        catalog::order(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ImportReport {
    pub rows: BTreeMap<ImportTable, u64>,
    #[serde(default)]
    pub summary_records_needing_reembedding: u64,
    #[serde(default)]
    pub summary_records_losing_passage_vectors: u64,
}

impl SqliteStore {
    pub async fn import_pg_dump(
        &self,
        path: PathBuf,
        cancellation: CancellationToken,
    ) -> Result<ImportReport, ImportError> {
        self.load_dump(path, cancellation, true).await
    }

    pub async fn preview_pg_dump(
        &self,
        path: PathBuf,
        cancellation: CancellationToken,
    ) -> Result<ImportReport, ImportError> {
        self.load_dump(path, cancellation, false).await
    }

    async fn load_dump(
        &self,
        path: PathBuf,
        cancellation: CancellationToken,
        apply: bool,
    ) -> Result<ImportReport, ImportError> {
        let index_key = self.summary_index_key;
        let conn = self.conn.clone();
        self.pending
            .spawn_blocking(move || {
                let file =
                    File::open(&path).map_err(|source| ImportError::File { path, source })?;
                import(
                    &mut conn.lock(),
                    BufReader::new(file),
                    &cancellation,
                    apply,
                    index_key.as_ref(),
                )
            })
            .await?
    }

    pub async fn pg_import_report(&self) -> Result<Option<ImportReport>, ImportError> {
        let conn = self.conn.clone();
        self.pending
            .spawn_blocking(move || {
                let text: Option<String> = conn
                    .lock()
                    .query_row(
                        "SELECT value FROM ros_meta WHERE key = ?1",
                        [RECEIPT_KEY],
                        |row| row.get(0),
                    )
                    .optional()?;
                text.map(|text| serde_json::from_str(&text))
                    .transpose()
                    .map_err(ImportError::from)
            })
            .await?
    }
}

fn import(
    conn: &mut Connection,
    reader: impl BufRead,
    cancellation: &CancellationToken,
    apply: bool,
    index_key: Option<&[u8; 32]>,
) -> Result<ImportReport, ImportError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
    let mut report = empty_report(&tx)?;
    let mut block: Option<columns::Block> = None;
    let mut seen = BTreeSet::new();
    let mut complete = false;
    let mut passages = PassageImport::default();
    let mut width = None;
    for (index, line) in reader.lines().enumerate() {
        check_cancelled(cancellation)?;
        let line = line?;
        let number = index + 1;
        if passages.read(&line, number)? {
            continue;
        }
        if let Some(active) = &mut block {
            if line == "\\." {
                block = None;
                continue;
            }
            active.insert(&tx, &line, number, &mut width)?;
            let count = report
                .rows
                .get_mut(&active.table)
                .ok_or(ImportError::Format {
                    line: number,
                    detail: "missing table report".into(),
                })?;
            *count += 1;
        } else if line.starts_with("COPY ") {
            if complete {
                return Err(ImportError::Format {
                    line: number,
                    detail: "COPY after dump completion".into(),
                });
            }
            let (name, fields) = columns::header(&line, number)?;
            if name == "ros_summary_passages" {
                passages.begin(fields, number)?;
                continue;
            }
            let active = columns::Block::parse(&tx, name, fields, number)?;
            if !seen.insert(active.table) {
                return Err(ImportError::DuplicateTable {
                    table: active.table,
                });
            }
            block = Some(active);
        } else if line == "-- PostgreSQL database dump complete" {
            complete = true;
        } else {
            copy::envelope(&line, number)?;
        }
    }
    if block.is_some() || passages.columns.is_some() || !complete || seen.is_empty() {
        return Err(ImportError::Truncated);
    }
    finish(
        &tx,
        width,
        &mut report,
        index_key,
        &passages.embedded_records,
    )?;
    check_cancelled(cancellation)?;
    if apply {
        tx.commit()?;
    } else {
        tx.rollback()?;
    }
    Ok(report)
}

#[derive(Default)]
struct PassageImport {
    columns: Option<PassageColumns>,
    seen: bool,
    embedded_records: BTreeSet<String>,
}

struct PassageColumns {
    record: Option<usize>,
    embedding: Option<usize>,
}

impl PassageImport {
    fn begin(&mut self, fields: &str, line: usize) -> Result<(), ImportError> {
        if self.seen {
            return Err(copy::invalid(line, "duplicate summary passages block"));
        }
        let position = |column| {
            fields
                .split(", ")
                .position(|name| name.trim_matches('"') == column)
        };
        self.columns = Some(PassageColumns {
            record: position("record_id"),
            embedding: position("embedding"),
        });
        self.seen = true;
        Ok(())
    }

    fn read(&mut self, text: &str, line: usize) -> Result<bool, ImportError> {
        let Some(columns) = &self.columns else {
            return Ok(false);
        };
        if text == "\\." {
            self.columns = None;
        } else if let (Some(record), Some(embedding)) = (columns.record, columns.embedding)
            && text
                .split('\t')
                .nth(embedding)
                .is_some_and(|value| value != "\\N")
            && let Some(record_id) = text.split('\t').nth(record)
            && let Some(id) = copy::field(record_id, line)?
        {
            self.embedded_records.insert(id);
        }
        Ok(true)
    }
}

fn empty_report(conn: &Connection) -> Result<ImportReport, ImportError> {
    let mut rows = BTreeMap::new();
    for table in ImportTable::iter() {
        let occupied: bool = conn.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {})",
                table.compatibility_name()
            ),
            [],
            |row| row.get(0),
        )?;
        if occupied {
            return Err(ImportError::NonEmpty { table });
        }
        rows.insert(table, 0);
    }
    Ok(ImportReport {
        rows,
        summary_records_needing_reembedding: 0,
        summary_records_losing_passage_vectors: 0,
    })
}

fn finish(
    tx: &rusqlite::Transaction<'_>,
    width: Option<usize>,
    report: &mut ImportReport,
    index_key: Option<&[u8; 32]>,
    embedded_records: &BTreeSet<String>,
) -> Result<(), ImportError> {
    let invalid: bool = tx.query_row(sql::SUMMARY_GRAPH, [], |row| row.get(0))?;
    if invalid {
        return Err(ImportError::SummaryGraph);
    }
    mark_index_repairs(tx, report, index_key, embedded_records)?;
    let foreign_key_violation = tx
        .query_row(
            "SELECT 1 FROM pragma_foreign_key_check LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if foreign_key_violation.is_some() {
        return Err(ImportError::ForeignKeys);
    }
    if let Some(width) = width {
        let supplied: Option<String> = tx
            .query_row(
                "SELECT value FROM ros_meta WHERE key = 'embed_dims'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match supplied {
            Some(value) if value != width.to_string() => return Err(ImportError::Vector),
            Some(_) => {}
            None => {
                tx.execute(
                    "INSERT INTO ros_meta(key, value) VALUES ('embed_dims', ?1)",
                    [width.to_string()],
                )?;
            }
        }
    }
    tx.execute(
        "INSERT INTO ros_meta(key, value) VALUES (?1, ?2)",
        [RECEIPT_KEY, &serde_json::to_string(report)?],
    )?;
    Ok(())
}

fn mark_index_repairs(
    tx: &rusqlite::Transaction<'_>,
    report: &mut ImportReport,
    index_key: Option<&[u8; 32]>,
    embedded_records: &BTreeSet<String>,
) -> Result<(), ImportError> {
    tx.execute(
        "UPDATE ros_summaries SET embedding = NULL, index_key = ?1, index_needs_repair = 1
         WHERE kind IN ('session', 'part')",
        [index_key.map(<[_; 32]>::as_slice)],
    )?;
    let mut records = tx.prepare(
        "SELECT id FROM ros_summaries WHERE kind IN ('session','part') AND index_needs_repair",
    )?;
    let ids = records.query_map([], |row| row.get::<_, String>(0))?;
    for id in ids {
        report.summary_records_needing_reembedding += 1;
        report.summary_records_losing_passage_vectors += u64::from(embedded_records.contains(&id?));
    }
    Ok(())
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<(), ImportError> {
    if cancellation.is_cancelled() {
        Err(ImportError::Cancelled)
    } else {
        Ok(())
    }
}
