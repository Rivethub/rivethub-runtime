use std::path::PathBuf;

use super::ImportTable;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("cannot read Postgres dump {path}: {source}")]
    File {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Postgres dump read failed: {0}")]
    Read(#[from] std::io::Error),
    #[error("Postgres import worker failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error("Postgres import refused: target table {table} is not empty")]
    NonEmpty { table: ImportTable },
    #[error("Postgres dump line {line}: {detail}")]
    Format { line: usize, detail: String },
    #[error("Postgres dump repeats table {table}")]
    DuplicateTable { table: ImportTable },
    #[error("Postgres dump is incomplete: expected COPY blocks and the pg_dump completion footer")]
    Truncated,
    #[error("Postgres import has an incomplete or inconsistent top-down summary graph")]
    SummaryGraph,
    #[error("Postgres import has missing foreign-key references")]
    ForeignKeys,
    #[error("Postgres import cancelled")]
    Cancelled,
    #[error("Postgres import database constraint or operation failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("Postgres import JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Store(#[from] crate::SqliteError),
    #[error("Postgres import vector must be finite, nonzero and have a consistent width")]
    Vector,
}
