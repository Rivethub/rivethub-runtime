use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use clap::Subcommand;
use config::ConfigLocation;
use config::Environment;
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(
    name = "rivethub-migrate",
    version,
    about = "applies the RivetHub schema migrations (postgres and sqlite)"
)]
struct Cli {
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Postgres {
        #[arg(long)]
        url: Option<String>,
    },
    Sqlite {
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long, value_name = "FILE")]
        import_pg_dump: Option<PathBuf>,
    },
    Status {
        #[command(subcommand)]
        backend: Backend,
    },
}

#[derive(Debug, Subcommand)]
enum Backend {
    Postgres {
        #[arg(long)]
        url: Option<String>,
    },
    Sqlite {
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

#[derive(Debug, Error)]
enum Failure {
    #[error("cannot inspect SQLite target: {0}")]
    Target(#[from] std::io::Error),
    #[error(transparent)]
    Import(#[from] memory_sqlite::import::ImportError),
    #[error(transparent)]
    Store(#[from] memory_sqlite::SqliteError),
    #[error("cannot encode import report: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the tokio runtime failed to start")]
    Runtime(#[source] std::io::Error),
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Migrate(#[from] migrate::Error),
    #[error("no postgres url — pass --url <url> or --config <file>")]
    PostgresUrl,
    #[error("no sqlite path — pass --path <file> or --config <file>")]
    SqlitePath,
    #[error("no config file — pass --config <file>")]
    NoConfig,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match runtime() {
        Ok(runtime) => match run(&cli, &runtime) {
            Ok(()) => ExitCode::SUCCESS,
            Err(failure) => {
                report(&failure);
                ExitCode::from(1)
            }
        },
        Err(failure) => {
            report(&failure);
            ExitCode::from(1)
        }
    }
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        runtime,
        reason = "rivethub-migrate is an operator binary and its own composition root, like the rivethub main"
    )
)]
fn runtime() -> Result<tokio::runtime::Runtime, Failure> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Failure::Runtime)
}

#[expect(
    clippy::print_stderr,
    reason = "the migrate binary is an operator tool: failures are reported on stderr and mapped to the exit code"
)]
fn report(failure: &Failure) {
    eprintln!("rivethub-migrate: {failure}");
}

#[expect(
    clippy::print_stdout,
    reason = "the migrate binary is an operator tool: the status report is its output"
)]
fn run(cli: &Cli, runtime: &tokio::runtime::Runtime) -> Result<(), Failure> {
    match &cli.command {
        Command::Postgres { url } => {
            let url = postgres_url(cli, url.clone())?;
            let status = runtime.block_on(migrate::postgres(&url))?;
            println!(
                "postgres: applied; current {}, required {}, pending {}",
                status.current, status.required, status.pending
            );
            Ok(())
        }
        Command::Sqlite {
            path,
            import_pg_dump,
        } => {
            let path = sqlite_path(cli, path.clone())?;
            if let Some(dump) = import_pg_dump {
                if !path.try_exists()? {
                    migrate::sqlite(&path)?;
                }
                let store = memory_sqlite::SqliteStore::open(&path)?;
                let report = runtime.block_on(
                    store.import_pg_dump(dump.clone(), tokio_util::sync::CancellationToken::new()),
                )?;
                println!("{}", serde_json::to_string(&report)?);
                return Ok(());
            }
            let status = migrate::sqlite(&path)?;
            println!(
                "sqlite {}: applied; current {}, required {}, pending {}",
                path.display(),
                status.current,
                status.required,
                status.pending
            );
            Ok(())
        }
        Command::Status { backend } => match backend {
            Backend::Postgres { url } => {
                let url = postgres_url(cli, url.clone())?;
                let status = runtime.block_on(migrate::postgres_status(&url))?;
                println!(
                    "postgres: current {}, required {}, pending {}",
                    status.current, status.required, status.pending
                );
                Ok(())
            }
            Backend::Sqlite { path } => {
                let path = sqlite_path(cli, path.clone())?;
                let status = migrate::sqlite_status(&path)?;
                println!(
                    "sqlite {}: current {}, required {}, pending {}",
                    path.display(),
                    status.current,
                    status.required,
                    status.pending
                );
                Ok(())
            }
        },
    }
}

fn postgres_url(cli: &Cli, url: Option<String>) -> Result<String, Failure> {
    if let Some(url) = url {
        return Ok(url);
    }
    let env = Environment::from_process();
    let config = load(cli, &env)?;
    config.memory_postgres_url(&env).ok_or(Failure::PostgresUrl)
}

fn sqlite_path(cli: &Cli, path: Option<PathBuf>) -> Result<PathBuf, Failure> {
    if let Some(path) = path {
        return Ok(path);
    }
    let env = Environment::from_process();
    let config = load(cli, &env)?;
    config
        .memory
        .as_ref()
        .and_then(|memory| memory.sqlite.as_ref())
        .and_then(|sqlite| sqlite.path.clone())
        .ok_or(Failure::SqlitePath)
}

fn load(cli: &Cli, env: &Environment) -> Result<config::Config, Failure> {
    let Some(location) = cli.config.clone().map(ConfigLocation::Current) else {
        return Err(Failure::NoConfig);
    };
    let loaded = config::Config::load(env, &location)?;
    report_loaded(&loaded);
    Ok(loaded.config)
}

#[expect(
    clippy::print_stderr,
    reason = "the migrate binary is an operator tool: each config warning is printed once on stderr before the config is used"
)]
fn report_loaded(loaded: &config::Loaded) {
    for finding in &loaded.warnings {
        eprintln!("rivethub-migrate: {}", finding_line(finding));
    }
    for notice in &loaded.notices {
        eprintln!("rivethub-migrate: {notice}");
    }
}

fn finding_line(finding: &config::Finding) -> String {
    if finding.path.is_empty() {
        finding.message.clone()
    } else {
        format!("[{}] {}", finding.path, finding.message)
    }
}
