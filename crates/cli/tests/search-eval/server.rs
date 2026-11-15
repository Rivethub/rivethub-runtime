use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use kernel::memory::MemoryStore;
use memory_sqlite::SqliteStore;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Parser)]
#[command(
    name = "search-eval-server",
    about = "Serves the RivetHub MCP memory tools over stdio from a SQLite memory database"
)]
struct Args {
    #[arg(value_name = "FILE")]
    sqlite: PathBuf,
}

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error("runtime: {0}")]
    Runtime(std::io::Error),
    #[error(transparent)]
    Store(#[from] memory_sqlite::SqliteError),
    #[error(transparent)]
    Memory(#[from] kernel::memory::MemoryStoreError),
    #[error(transparent)]
    Mcp(#[from] mcp::Error),
}

fn main() -> ExitCode {
    let args = Args::parse();
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Failure::Runtime)
        .and_then(|runtime| runtime.block_on(serve(&args)));
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => last_report(&error),
    }
}

async fn serve(args: &Args) -> Result<(), Failure> {
    let store: Arc<dyn MemoryStore> = Arc::new(SqliteStore::open(&args.sqlite)?);
    let shutdown = CancellationToken::new();
    let server = mcp::Server::new(Arc::clone(&store), shutdown.clone())?;
    let outcome = mcp::transport::serve_stdio(server, shutdown).await;
    let closed = store.close().await;
    outcome?;
    closed?;
    Ok(())
}

#[expect(clippy::let_underscore_must_use)]
#[expect(clippy::let_underscore_untyped)]
fn last_report(error: &Failure) -> ExitCode {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "search-eval-server: {error}");
    let _ = stderr.flush();
    ExitCode::from(2)
}
