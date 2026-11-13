use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("cannot read this node's hostname: {0}")]
    Hostname(#[from] nix::errno::Errno),
    #[error("this node's hostname is not Unicode")]
    HostnameEncoding,
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error("update requires Linux x86_64 or aarch64, or macOS")]
    Platform,
    #[error("invalid release version: {0}")]
    Version(String),
    #[error("the release metadata GitHub answered is not a release")]
    Release,
    #[error("no published release is tagged {0}")]
    NotPublished(String),
    #[error("no published release found")]
    NoRelease,
    #[error("release {0} is a draft, not a published release")]
    Draft(String),
    #[error("the latest release {0} is a prerelease; use --channel beta or --version {0}")]
    Prerelease(String),
    #[error("asked for release {asked} but GitHub answered {answered}")]
    Mismatch { asked: String, answered: String },
    #[error("release {tag} does not have exactly one asset named {name}")]
    Asset { tag: String, name: String },
    #[error("cannot read the service unit {}: {source}", path.display())]
    Unit {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot restart the local node: {action}: {source}")]
    Local {
        action: String,
        #[source]
        source: Box<crate::Error>,
    },
    #[error(
        "the local node (PID {pid}) did not stop within {seconds}s of SIGTERM; the new binary is installed, run `rh stop` and `rh local --ensure`"
    )]
    Stopping { pid: i32, seconds: u64 },
    #[error("release archive must contain exactly one regular file named {0}")]
    Archive(&'static str),
    #[error("release download failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("release asset is too large")]
    AssetSize,
    #[error("release checksum manifest is invalid")]
    Manifest,
    #[error("SHA-256 verification failed for {0}")]
    Checksum(String),
    #[error("update path is outside its root or is not a regular file: {}", .0.display())]
    Path(PathBuf),
    #[error("another update is in progress")]
    Busy,
    #[error("source updates require RIVETHUB_INSTALL_ROOT pointing to a checkout")]
    Source,
    #[error("source checkout must be clean and on branch {0}")]
    Branch(&'static str),
    #[error("source checkout does not match the requested version")]
    Revision,
    #[error("update I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("update task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error("update command failed: {0}")]
    Command(#[from] crate::service::ServiceError),
    #[error("update cancelled")]
    Cancelled,
    #[error(transparent)]
    Account(#[from] gateway::account::AccountError),
    #[error("rivethub.cloud installs its latest release; pass --source github to install a tag")]
    CloudVersion,
    #[error("rivethub.cloud has no {0} release for this platform")]
    NoCloudRelease(wire::gateway::account::ReleaseChannel),
}
