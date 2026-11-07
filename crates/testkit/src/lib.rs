pub mod capture;
pub mod capture_pg;
pub mod cases;
pub mod decisions;
pub mod facts;
pub mod fakes;
pub mod filing;
pub mod filing_bulk;
pub mod graphile_schema;
pub mod http;
pub mod json;
pub mod mcp_text;
pub mod memory;
mod memory_request;
pub mod naming;
pub mod paths;
pub mod pgdump;
pub mod ports;
pub mod provider;
pub mod session_delete;
pub mod sqlite;
pub mod stub;
pub mod sweeps;
pub mod task_memory;
pub mod tasks;
pub mod tidy;
pub mod topic_pages;
pub mod topics;
pub mod wiki_bounded;
pub mod wiki_facts;
pub mod wiki_golden;
pub mod wiki_lineage;
pub mod wiki_mentions;
pub mod wiki_routes;
pub mod ws;

use std::path::Path;
use std::path::PathBuf;

pub use capture::HookFile;
pub use capture::HookOutcome;
pub use capture::HookPost;
pub use capture_pg::PgOutcome;
pub use capture_pg::PgStep;
pub use cases::Thrown;
pub use fakes::FakeHarness;
pub use fakes::FakeJobQueue;
pub use fakes::FakeMux;
pub use fakes::FakeProvider;
pub use fakes::FakeReader;
pub use fakes::FakeSessionDeletion;
pub use fakes::FakeTool;
pub use fakes::Script;
pub use graphile_schema::install_graphile_worker;
pub use http::Handshake;
pub use http::Request;
pub use http::Response;
pub use json::Json;
pub use json::Masks;
pub use json::json_eq;
pub use json::json_masked_eq;
pub use json::masked_eq;
pub use json::substitute;
pub use memory::FakeMemoryStore;
pub use memory::MemoryRequest;
pub use naming::translate_recorded_product_names;
pub use paths::golden_path;
pub use paths::recording;
pub use pgdump::PgNamer;
pub use pgdump::capture_tx;
pub use pgdump::dump_rivethub;
pub use pgdump::queue_present;
pub use provider::B8_OMITTED_REQUEST_FIELDS;
pub use provider::Canned;
pub use provider::CapturedRequest;
pub use provider::ProviderTurn;
pub use provider::WireDelta;
pub use provider::strip_b8_deltas;
pub use sqlite::FixtureCopy;
pub use sqlite::FixtureDb;
pub use sqlite::copy_fixture;
pub use sqlite::open_fixture;
pub use sqlite::query_json;
pub use sqlite::rows_eq;
pub use sqlite::sqlite_version;
pub use stub::Stub;
pub use stub::stub_server;
pub use task_memory::TaskMemory;
pub use ws::Frame;
pub use ws::WsCall;
pub use ws::WsReply;
pub use ws::WsRequest;
pub use ws::WsSent;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{id}: {detail}")]
pub struct Mismatch {
    pub id: String,
    pub detail: String,
}

impl Mismatch {
    pub fn new(id: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            detail: detail.into(),
        }
    }
}

impl From<regex::Error> for Mismatch {
    fn from(error: regex::Error) -> Self {
        Self::new("recording mask", error.to_string())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("recording {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("recording {}: {message}", path.display())]
    Format { path: PathBuf, message: String },
    #[error("invalid recording pattern: {0}")]
    Regex(#[from] regex::Error),
    #[error("invalid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("invalid boolean: {0}")]
    Boolean(#[from] std::str::ParseBoolError),
    #[error("invalid number: {0}")]
    Number(#[from] std::num::ParseIntError),
    #[error("invalid timestamp: {0}")]
    Timestamp(#[from] chrono::ParseError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

impl Error {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    pub(crate) fn format(path: &Path, message: impl Into<String>) -> Self {
        Self::Format {
            path: path.to_path_buf(),
            message: message.into(),
        }
    }
}

pub(crate) fn read_text(path: &Path) -> Result<String, Error> {
    std::fs::read_to_string(path).map_err(|source| Error::io(path, source))
}

pub(crate) fn parse_json<T>(path: &Path, text: &str) -> Result<T, Error>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_str(text).map_err(|error| Error::format(path, error.to_string()))
}

pub const OWNER: &str = "owner";

pub fn owner() -> Result<kernel::owner::Owner, kernel::owner::InvalidOwner> {
    kernel::owner::Owner::new(OWNER.to_owned())
}
