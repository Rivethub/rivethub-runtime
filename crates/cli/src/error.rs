use config::NO_MODEL_NOTE;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Update(#[from] crate::update::UpdateError),
    #[error("no memory backend configured")]
    MemoryBackend,
    #[error("memory.capture.gateway.token is not a valid header value")]
    CaptureToken,
    #[error(
        "the listener on port {port} is not owned by this user, so the owner token was not sent; stop whatever holds the port and run `rh local --ensure`"
    )]
    ListenerNotOurs { port: u16 },
    #[error("the gateway is not running on port {port}; run `rh local --ensure`")]
    GatewayNotRunning { port: u16 },
    #[error(transparent)]
    LocalOwnerToken(#[from] gateway::local_owner::TokenError),
    #[error("{0}")]
    GatewayCertificate(&'static str),
    #[error(transparent)]
    Service(#[from] crate::service::ServiceError),
    #[error(
        "No config found. Write ~/.rivethub/config.yaml (rh local writes a standalone one) or set RIVETHUB_CONFIG"
    )]
    NoConfig,
    #[error(
        "No config found. Write ~/.rivethub/config.yaml (rh local writes a standalone one) or use --config <path>"
    )]
    StartConfig,
    #[error(transparent)]
    State(#[from] config::StateError),
    #[error("Failed to parse config: {0}")]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sharing(#[from] crate::memory::ShareError),
    #[error(transparent)]
    ShareServer(#[from] crate::share_server::ServerError),
    #[error(transparent)]
    Drop(#[from] crate::memory::DropError),
    #[error(transparent)]
    Capture(#[from] capture::writer::WriteError),
    #[error(transparent)]
    Harness(#[from] kernel::harness_setup::SetupError),
    #[error("blocking command task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error("invalid PID file: expected a single PID greater than 1")]
    Pid,
    #[error("process signal failed: {0}")]
    Signal(#[from] nix::errno::Errno),
    #[error("gateway health request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error(
        "a gateway (pid {pid}) holds this state directory but could not be reached, so the token file was kept; run `rh stop`, then `rh auth rotate` again"
    )]
    AuthRotateUnreachable { pid: i32 },
    #[error("the gateway refused the token rotation with HTTP {status}")]
    AuthRotate { status: u16 },
    #[error(
        "the gateway refused to mint a login code with HTTP {status}; the token file may be stale, restart the gateway or run `rh auth rotate`"
    )]
    AuthOpen { status: u16 },
    #[error(
        "This node is running, and it has no web app installed.\nAgents read memory over MCP. The app is in preview."
    )]
    NoWebApp,
    #[error("{NO_MODEL_NOTE}")]
    NoModel,
    #[error("invalid gateway URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("gateway host or port is invalid")]
    Address,
    #[error("schema verifier failed: rivethub-migrate: {0}")]
    Verifier(#[from] migrate::Error),
    #[error(
        "schema verifier failed: rivethub-migrate: no postgres url — pass --url <url> or --config <file>"
    )]
    PostgresUrl,
    #[error(
        "schema verifier failed: rivethub-migrate: no sqlite path — pass --path <file> or --config <file>"
    )]
    SqlitePath,
    #[error("{0}")]
    Migrate(migrate::Error),
    #[error("rh local runs on SQLite; this config selects Postgres")]
    PostgresBackend,
    #[error("rh local needs memory.sqlite.path set to a database file")]
    LocalDatabase,
    #[error("rh local --port must be between 1 and 65535")]
    GatewayPort,
    #[error(
        "{} binds the gateway to {host}, which needs TLS. Run `rh local --no-lan` to serve on 127.0.0.1 only, set gateway.host in that file to 127.0.0.1, or set gateway.tls_cert and gateway.tls_key (or RIVETHUB_TLS_CERT and RIVETHUB_TLS_KEY) to the node's certificate",
        path.display()
    )]
    LanNeedsTls {
        path: std::path::PathBuf,
        host: String,
    },
    #[error(
        "rh local was asked to keep this hub off the cloud (--no-cloud or RIVETHUB_CLOUD=off), but {} does not turn it off; add `cloud: {{disabled: true}}` to that file and run rh local again",
        path.display()
    )]
    CloudLeftOn { path: std::path::PathBuf },
    #[error(
        "rh local was asked to keep this hub off the cloud (--no-cloud or RIVETHUB_CLOUD=off), but the node at {origin} is running without this hub's cloud policy; restart it (stop the node, then run rh local again) so the policy applies"
    )]
    NodeCloudLeftOn { origin: String },
    #[error("refusing to replace the config at {}", path.display())]
    ConfigPresent { path: std::path::PathBuf },
    #[error("the node claim was already released")]
    ClaimMissing,
    #[error("JSON output failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("command cancelled")]
    Cancelled,
    #[error("the operating-system hostname is not Unicode")]
    HostName,
    #[error("This command needs a datahub (Postgres).")]
    PostgresUrlRequired,
    #[error("Unknown option: {0}")]
    UnknownOption(String),
    #[error("--task requires a task identifier (e.g. extract-wiki)")]
    TaskRequired,
    #[error("--error requires a substring to match against last_error")]
    ErrorSubstringRequired,
    #[error(
        "At least one --task <identifier> is required (e.g. --task extract-wiki). Refusing to reset every dead job without an explicit task filter."
    )]
    RetryTaskFilter,
    #[error(
        "At least one --task <identifier> is required (e.g. --task extract-wiki). Refusing to requeue every dead job without an explicit task filter."
    )]
    RequeueTaskFilter,
    #[error("--task {task} is not an allowed memory task ({allowed})")]
    TaskNotAllowed { task: String, allowed: &'static str },
    #[error("requeue accepts one --task")]
    OneTask,
    #[error("--limit requires a positive integer")]
    LimitPositive,
    #[error("--limit requires an integer")]
    LimitInteger,
    #[error("--out requires a file path")]
    OutRequired,
    #[error("--since requires an ISO timestamp")]
    SinceRequired,
    #[error("--min-bytes requires a byte count: {0}")]
    RecompileBytes(String),
    #[error("--slug requires a wiki page slug: {0}")]
    RecompileSlug(String),
    #[error("invalid --since timestamp: {0}")]
    SinceInvalid(String),
    #[error("refusing to write gzip to a TTY (redirect stdout or pass --out <file>)")]
    GzipTty,
    #[error("import requires a file path")]
    ImportPath,
    #[error("import accepts one file path")]
    ImportOne,
    #[error("merge-store: {0}")]
    MergeStore(String),
    #[error("reembed runs on a SQLite memory file; pass --sqlite <file>")]
    ReembedSqlite,
    #[error("--sqlite requires a file path")]
    SqliteRequired,
    #[error("unexpected argument: {0}")]
    Unexpected(String),
    #[error("output path is a symlink")]
    OutputSymlink,
    #[error("output directory does not exist")]
    OutputDirectory,
    #[error("output path has no file name")]
    OutputName,
    #[error("hostname is unavailable")]
    HostUnavailable,
    #[error("invalid RIVETHUB_PG_URL: {0}")]
    MemoryUrl(url::ParseError),
    #[error("RIVETHUB_PG_URL has no host")]
    MemoryHost,
    #[error("rh logs accepts one agent identifier")]
    OneAgent,
    #[error("--lines requires an integer")]
    LinesInteger,
    #[error("--level requires a syslog level")]
    LevelRequired,
    #[error("unknown syslog level: {0}")]
    LevelUnknown(String),
    #[error("--since requires a time")]
    JournalSince,
    #[error("--grep requires text")]
    GrepRequired,
    #[error("unknown export source: {0}")]
    UnknownDumpSource(String),
    #[error("memory dump is not UTF-8")]
    DumpEncoding,
    #[error("export host is empty")]
    DumpHost,
    #[error(transparent)]
    MemoryPostgres(#[from] memory_postgres::Error),
    #[error(transparent)]
    MemorySqlite(#[from] memory_sqlite::SqliteError),
    #[error(transparent)]
    MemoryImport(#[from] memory_sqlite::import::ImportError),
    #[error(transparent)]
    WikiStore(#[from] kernel::wiki::builder::WikiStoreError),
    #[error(transparent)]
    JobQueue(#[from] kernel::queue::JobQueueError),
    #[error("rh memory cleanup runs as --dry-run or --propose")]
    CleanupMode,
    #[error(
        "rh memory cleanup --propose needs --database <url> naming the database to write the proposals to"
    )]
    CleanupDatabase,
    #[error("refusing to write cleanup proposals to the node's live memory database")]
    CleanupLive,

    #[error(transparent)]
    Lineage(#[from] kernel::transcript::TranscriptReaderError),
    #[error(transparent)]
    Tidy(#[from] kernel::tidy::TidyError),
    #[error("rh memory cleanup needs --out <dir> for its checkpoints and report")]
    CleanupOut,
    #[error("rh memory cleanup needs --model-url <url> and --model <name>")]
    CleanupModel,
    #[error(
        "rh memory backfill-titles names titles with both --model <name> and --model-url <url>"
    )]
    TitlesModel,
    #[error("This command needs a datahub (Postgres).")]
    TitlesSqlite,
    #[error("{0} requires a value")]
    CleanupValue(&'static str),
    #[error("{0} requires a positive integer")]
    CleanupNumber(&'static str),
    #[error(
        "the {stage} stage stopped with {failed} failed job(s); rerun the same command to resume"
    )]
    CleanupIncomplete { stage: &'static str, failed: usize },
    #[error(transparent)]
    CleanupSource(#[from] kernel::cleanup::CleanupError),
    #[error(transparent)]
    Topic(#[from] kernel::topics::TopicError),
    #[error(transparent)]
    Worker(#[from] workers::WorkerError),
    #[error(transparent)]
    ProviderBuild(#[from] providers::BuildError),
    #[error("--session requires a conversation id")]
    SessionRequired,
    #[error("no session this node can export has the id {0}")]
    BundleUnknown(String),
    #[error("--out {0} is a directory; name the bundle file")]
    BundleOutDirectory(String),
    #[error("the users file {path} is not a users registry: {source}")]
    BundleUsers {
        path: String,
        source: serde_json::Error,
    },
    #[error("{path} is a raw bundle that was never sanitized; pass --force to ingest it as it is")]
    BundleRaw { path: String },
    #[error(transparent)]
    Bundle(#[from] capture::bundle::BundleError),
    #[error("{path}: {source}; nothing was imported")]
    BundleBlocked {
        path: String,
        source: capture::receive::Blocked,
    },
    #[error(transparent)]
    Sanitize(#[from] capture::sanitize::SanitizeError),
    #[error(transparent)]
    SanitizeKey(#[from] capture::sanitize::KeyError),
    #[error(transparent)]
    Share(#[from] capture::share::ShareError),
    #[error("--mapping requires a file")]
    MappingRequired,
    #[error("--resolve requires the report file sanitize wrote")]
    ResolveRequired,
    #[error(
        "--resolve takes the report, an optional bundle, --mapping and --learn; it does not take --out, --decide, --block or the --session options"
    )]
    ResolveAlone,
    #[error("--learn adds map marks to the mapping file and needs --resolve <report>")]
    LearnWithoutResolve,
    #[error(
        "{0} does not end in .report.md, so name the bundle to resolve: rh memory sanitize <bundle> --resolve <report>"
    )]
    ResolveBundle(String),
    #[error("the report {path} cannot be resolved: {source}")]
    Report {
        path: String,
        source: capture::sanitize::marks::ReportError,
    },
    #[error("--block makes the decision-model review block ingest and needs --decide")]
    BlockWithoutDecide,
    #[error("{0} requires a value")]
    OptionValue(&'static str),
    #[error("{0} is not a conversation id; give the UUID of a stored session")]
    ConversationId(String),
    #[error("--redact-term takes <conversation id>=<term>, not {0}")]
    RedactTermForm(String),
    #[error("{flag} {id} names a session no --session gives; choose it with --session {id} first")]
    ChoiceWithoutSession { flag: &'static str, id: String },
    #[error("--sqlite names the store --session reads from, so it needs --session")]
    SqliteWithoutSession,
    #[error(
        "--session reads the chosen sessions from the store, so it does not take a bundle file; give one or the other"
    )]
    SessionWithBundle,
    #[error(transparent)]
    Choice(#[from] kernel::memory::share::ChoiceError),
    #[error(transparent)]
    SessionShare(#[from] kernel::memory::share::ShareFailure),
    #[error(transparent)]
    Owner(#[from] kernel::owner::InvalidOwner),
    #[error(
        "{path} withholds {count} flagged chunk(s) until you mark them; mark them in the report and run rh memory sanitize --resolve <report>, or pass --force to ingest it with the chunks withheld"
    )]
    BundleFlags { path: String, count: usize },
    #[error(
        "cannot read the local review file {path} ({source}); sanitize writes it beside the report, so resolve on the machine and in the folder where sanitize ran"
    )]
    ReviewLocal {
        path: String,
        source: std::io::Error,
    },
}
