mod corpus;
mod mcp;
mod report;
mod score;
mod set;
mod shown;
mod store;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use chrono::Utc;
use clap::Parser;
use kernel::memory::Limit;
use kernel::memory::MemoryScope;
use kernel::memory::SearchQuery;
use memory_sqlite::SqliteStore;

use score::Metrics;
use score::Scored;
use set::Baseline;
use set::EvalQuery;
use set::QuerySet;
use shown::Ranking;

#[derive(Debug, Parser)]
#[command(
    name = "search-eval",
    about = "Scores memory search against a query set and gates it on a saved baseline"
)]
struct Args {
    #[arg(long, value_name = "FILE")]
    queries: PathBuf,
    #[arg(long, value_name = "FILE")]
    baseline: Option<PathBuf>,
    #[arg(
        long,
        value_name = "FILE",
        conflicts_with = "corpus",
        required_unless_present_any = ["corpus", "server"]
    )]
    sqlite: Option<PathBuf>,
    #[arg(long, value_name = "FILE")]
    corpus: Option<PathBuf>,
    #[arg(long, requires = "baseline", conflicts_with = "save_baseline")]
    check: bool,
    #[arg(long, requires = "baseline")]
    save_baseline: bool,
    #[arg(last = true, value_name = "SERVER")]
    server: Vec<OsString>,
}

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot copy {path}: {source}")]
    Copy {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not a valid file: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("pass --sqlite FILE or --corpus FILE")]
    NoDatabase,
    #[error("baseline: {0}")]
    Encode(serde_json::Error),
    #[error("scratch directory: {0}")]
    Scratch(std::io::Error),
    #[error("runtime: {0}")]
    Runtime(std::io::Error),
    #[error("stdout: {0}")]
    Stdout(std::io::Error),
    #[error(transparent)]
    Migrate(#[from] migrate::Error),
    #[error("query {id}: {source}")]
    Search {
        id: String,
        source: memory_sqlite::SqliteError,
    },
    #[error(transparent)]
    Store(#[from] memory_sqlite::SqliteError),
    #[error("the query set is too large to score: {0}")]
    Count(#[from] std::num::TryFromIntError),
    #[error("pass the MCP server command after --")]
    NoServer,
    #[error("cannot start the MCP server: {0}")]
    Spawn(std::io::Error),
    #[error("MCP handshake: {0}")]
    Initialize(Box<rmcp::service::ClientInitializeError>),
    #[error("MCP: {0}")]
    Service(#[from] rmcp::ServiceError),
    #[error("MCP server stopped unexpectedly: {0}")]
    Quit(String),
    #[error("MCP server stopped: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("the MCP server does not serve memory_search; it offers: {offered}")]
    NoTool { offered: String },
    #[error("query {id}: memory_search answered with an error: {text}")]
    Tool { id: String, text: String },
    #[error("memory_search arguments: {0}")]
    Arguments(serde_json::Error),
    #[error("result pattern: {0}")]
    Pattern(#[from] regex::Error),
}

enum Verdict {
    Pass,
    Fail,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Failure::Runtime)
        .and_then(|runtime| runtime.block_on(run(&args)));
    match outcome {
        Ok(Verdict::Pass) => ExitCode::SUCCESS,
        Ok(Verdict::Fail) => ExitCode::FAILURE,
        Err(error) => last_report(&error),
    }
}

async fn run(args: &Args) -> Result<Verdict, Failure> {
    let set = QuerySet::load(&args.queries)?;
    let opened = match (&args.sqlite, &args.corpus) {
        (Some(pinned), _) => Some(store::pinned(pinned)?),
        (None, Some(corpus)) => Some(store::seeded(corpus).await?),
        (None, None) => None,
    };
    let scored = match (args.server.is_empty(), &opened) {
        (false, _) => mcp::evaluate(&args.server, opened.as_ref(), &set).await?,
        (true, Some(opened)) => evaluate(&opened.store, &set).await?,
        (true, None) => return Err(Failure::NoDatabase),
    };
    let metrics = Metrics::of(&scored, set.limit.get())?;
    let mut out = report::queries(&scored);
    out.extend(report::summary(&metrics));
    let verdict = match (&args.baseline, args.check, args.save_baseline) {
        (Some(path), _, true) => {
            Baseline::save(path, &metrics.baseline())?;
            out.push(String::new());
            out.push(format!("Baseline written to {}", path.display()));
            Verdict::Pass
        }
        (Some(path), true, false) => {
            let problems = metrics.regressions(&Baseline::load(path)?, set.tolerance);
            out.extend(report::regressions(&problems));
            if problems.is_empty() {
                Verdict::Pass
            } else {
                Verdict::Fail
            }
        }
        _ if scored.iter().any(Scored::missed) => Verdict::Fail,
        _ => Verdict::Pass,
    };
    let mut stdout = std::io::stdout().lock();
    for line in out {
        writeln!(stdout, "{line}").map_err(Failure::Stdout)?;
    }
    stdout.flush().map_err(Failure::Stdout)?;
    Ok(verdict)
}

async fn evaluate(store: &SqliteStore, set: &QuerySet) -> Result<Vec<Scored>, Failure> {
    let mut scored = Vec::with_capacity(set.queries.len());
    for query in &set.queries {
        let now = set.clock.unwrap_or_else(Utc::now);
        let started = Instant::now();
        let outcome = store
            .search_at(search(query, set.limit.get()), now, &Utc)
            .await
            .map_err(|source| Failure::Search {
                id: query.id.clone(),
                source,
            })?;
        scored.push(Scored::new(
            query,
            &Ranking::of(&outcome),
            &BTreeMap::new(),
            started.elapsed(),
        )?);
    }
    Ok(scored)
}

fn search(query: &EvalQuery, limit: u32) -> SearchQuery {
    SearchQuery {
        text: query.query.clone(),
        mode: query.mode,
        scope: MemoryScope::Both,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(limit), limit, 1..=limit),
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
    }
}

fn read(path: &Path) -> impl FnOnce(std::io::Error) -> Failure {
    let path = path.to_path_buf();
    move |source| Failure::Read { path, source }
}

#[expect(clippy::let_underscore_must_use)]
#[expect(clippy::let_underscore_untyped)]
fn last_report(error: &Failure) -> ExitCode {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "search-eval: {error}");
    let _ = stderr.flush();
    ExitCode::from(2)
}
