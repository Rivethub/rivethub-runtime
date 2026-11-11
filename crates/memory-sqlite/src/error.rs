use std::num::TryFromIntError;
use std::path::PathBuf;

use kernel::memory::MemoryStoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SqliteError {
    #[error("the memory file at {path} could not be opened")]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("the memory file at {path} is not initialised — run rh db migrate --path {path}")]
    NotInitialised { path: PathBuf },
    #[error(
        "the memory schema is behind: the file reports version {found}, this build requires {required} — run rh db migrate --path {path}"
    )]
    SchemaBehind {
        path: PathBuf,
        found: i64,
        required: i64,
    },
    #[error("a memory schema pragma failed")]
    Pragma(#[source] rusqlite::Error),
    #[error("the memory schema version {found} is not supported by this build")]
    UnsupportedSchema { found: i64 },
    #[error("the blocking memory database worker did not complete")]
    Join(#[source] tokio::task::JoinError),
    #[error("memory {operation} failed")]
    Query {
        operation: &'static str,
        #[source]
        source: rusqlite::Error,
    },
    #[error("the {column} JSON could not be encoded")]
    Encode {
        column: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("stored {column} is not a JSON object")]
    StoredJson {
        column: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("stored {column} \"{value}\" is not a timestamp")]
    StoredTimestamp {
        column: &'static str,
        value: String,
        #[source]
        source: chrono::ParseError,
    },
    #[error("a stored count does not fit an unsigned count")]
    Count(#[source] TryFromIntError),
    #[error("job {id} does not exist")]
    MissingJob { id: String },
    #[error("the job clock overflowed")]
    Clock,
    #[error("stored job state \"{value}\" is not queued, running, or dead")]
    StoredState { value: String },
    #[error("stored {column} \"{value}\" is not a valid value")]
    StoredValue { column: &'static str, value: String },
    #[error("embed job for {id} was already queued")]
    EmbedAlreadyQueued { id: String },
    #[error("job payload is not a JSON object")]
    PayloadNotObject,
    #[error("{message}")]
    ClaimRejected { message: String },
    #[error("job insert was rejected without a job_key")]
    InsertRejected,
    #[error("stored payload is not job JSON")]
    StoredPayload {
        #[source]
        source: serde_json::Error,
    },
    #[error("since must precede before")]
    InvertedRange,
    #[error("stored session key \"{value}\" is not a session id")]
    StoredSession { value: String },
    #[error("stored topic id \"{value}\" is not a topic id")]
    StoredTopic { value: String },
    #[error("stored tag entity type \"{value}\" is not conversation or summary")]
    StoredTagType { value: String },
    #[error("invalid search regular expression: {0}")]
    Regex(#[from] regex::Error),
    #[error("memory search cancelled")]
    SearchCancelled,
    #[error("memory export cancelled")]
    ExportCancelled,
    #[error("stored embedding is not a finite float32 vector of the query width")]
    StoredEmbedding,
    #[error("memory synthesis requires a configured summarizer endpoint")]
    SynthesisUnconfigured,
    #[error("search hit {id} was not updated")]
    MissingHit { id: String },
    #[error("schema v7 has no {table} table")]
    MissingTable { table: &'static str },
    #[error("tag tables are missing")]
    TagTablesMissing,
    #[error("{message}")]
    Invalid { message: String },
    #[error("embedding width is {got}, the column is {expected}")]
    Width { got: usize, expected: usize },
    #[error(
        "stored vectors were written by embedding model {stored}, the configured model is {configured}; run rh memory reembed"
    )]
    EmbedModel { stored: String, configured: String },
}

impl SqliteError {
    pub(crate) fn query(operation: &'static str) -> impl FnOnce(rusqlite::Error) -> Self {
        move |source| Self::Query { operation, source }
    }
}

impl From<SqliteError> for MemoryStoreError {
    fn from(error: SqliteError) -> Self {
        match error {
            SqliteError::InvertedRange
            | SqliteError::Regex(_)
            | SqliteError::SynthesisUnconfigured
            | SqliteError::Invalid { .. }
            | SqliteError::Width { .. }
            | SqliteError::EmbedModel { .. } => Self::InvalidRequest {
                message: error.to_string(),
            },
            SqliteError::StoredEmbedding
            | SqliteError::Query { .. }
            | SqliteError::StoredJson { .. }
            | SqliteError::StoredTimestamp { .. }
            | SqliteError::Count(_)
            | SqliteError::Encode { .. }
            | SqliteError::MissingJob { .. }
            | SqliteError::Clock
            | SqliteError::StoredState { .. }
            | SqliteError::StoredValue { .. }
            | SqliteError::EmbedAlreadyQueued { .. }
            | SqliteError::PayloadNotObject
            | SqliteError::ClaimRejected { .. }
            | SqliteError::InsertRejected
            | SqliteError::StoredPayload { .. }
            | SqliteError::StoredTagType { .. }
            | SqliteError::StoredSession { .. }
            | SqliteError::StoredTopic { .. }
            | SqliteError::MissingHit { .. }
            | SqliteError::MissingTable { .. } => Self::Query {
                message: error.to_string(),
                source: Box::new(error),
            },
            SqliteError::TagTablesMissing => Self::TagTablesMissing,
            SqliteError::SearchCancelled
            | SqliteError::ExportCancelled
            | SqliteError::Open { .. }
            | SqliteError::NotInitialised { .. }
            | SqliteError::SchemaBehind { .. }
            | SqliteError::Pragma(_)
            | SqliteError::UnsupportedSchema { .. }
            | SqliteError::Join(_) => Self::Unavailable(Box::new(error)),
        }
    }
}
