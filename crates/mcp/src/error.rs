use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Format(#[from] std::fmt::Error),
    #[error("invalid arguments: {0}")]
    Arguments(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("operation refused: {0}")]
    Refused(&'static str),
    #[error("operation cancelled")]
    Cancelled,
    #[error(transparent)]
    Memory(#[from] kernel::memory::MemoryStoreError),
    #[error(transparent)]
    WikiSlug(#[from] kernel::wiki::page::WikiSlugError),
    #[error("wiki page not found: {0}")]
    WikiMissing(String),
    #[error("wiki redirect cycle or chain exceeds 16 lookups")]
    WikiRedirect,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Unix socket is listening at {path}")]
    SocketListening { path: std::path::PathBuf },
    #[error("Unix socket path is not a socket: {path}")]
    SocketNotSocket { path: std::path::PathBuf },
    #[error("Unix socket path changed during startup: {path}")]
    SocketChanged { path: std::path::PathBuf },
    #[error("Unix socket operation failed at {path}: {source}")]
    SocketIo {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("Unix socket liveness check timed out at {path}")]
    SocketProbeTimeout { path: std::path::PathBuf },
    #[error(transparent)]
    Task(#[from] tokio::task::JoinError),
    #[error("MCP initialization failed: {0}")]
    Initialize(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    Service(#[from] rmcp::service::ServiceError),
    #[error(transparent)]
    Path(#[from] crate::sessions::PathRefusal),
    #[error(transparent)]
    Share(#[from] kernel::memory::share::ShareFailure),
    #[error(
        "refused: {} is a raw bundle that was never sanitized; run session_sanitize on the raw bundle first", .0.display()
    )]
    RawBundle(std::path::PathBuf),
    #[error(
        "refused: {} claims to be sanitized, but {count} secret pattern match(es) remain ({first}); sanitize the raw bundle again", path.display()
    )]
    BundleSecrets {
        path: std::path::PathBuf,
        count: usize,
        first: String,
    },
    #[error(
        "refused: {} withholds {count} flagged chunk(s) until they are marked; mark them in the report and run rh memory sanitize --resolve <report>", path.display()
    )]
    BundleWithheld {
        path: std::path::PathBuf,
        count: usize,
    },
}

pub fn required<T>(value: Option<T>, field: &str) -> Result<T, Error> {
    value.ok_or_else(|| Error::Invalid(format!("{field} is required")))
}
