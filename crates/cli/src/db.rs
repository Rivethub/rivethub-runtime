use std::path::PathBuf;

use clap::{Args, Subcommand};
use config::{Config, ConfigLocation, Environment, Profile};
use tokio_util::sync::CancellationToken;

use crate::{Error, Report, configuration};

#[derive(Debug, Args)]
pub struct DbArgs {
    #[arg(short = 'c', long, global = true)]
    pub config: Option<PathBuf>,
    #[arg(long, global = true, conflicts_with = "path")]
    pub url: Option<String>,
    #[arg(long, global = true)]
    pub path: Option<PathBuf>,
    #[command(subcommand)]
    pub command: DbCommand,
}

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum DbCommand {
    Migrate,
    Status,
}

#[derive(Debug)]
pub(crate) enum Target {
    Postgres(String),
    Sqlite(PathBuf),
}

impl Target {
    pub(crate) fn configured(config: &Config, env: &Environment) -> Result<Self, Error> {
        if config.sqlite_selected() {
            return config
                .memory
                .as_ref()
                .and_then(|memory| memory.sqlite.as_ref())
                .and_then(|sqlite| sqlite.path.clone())
                .map(Self::Sqlite)
                .ok_or(Error::SqlitePath);
        }
        config
            .memory_postgres_url(env)
            .map(Self::Postgres)
            .ok_or(Error::MemoryBackend)
    }

    pub(crate) fn arguments(&self) -> Vec<std::ffi::OsString> {
        match self {
            Self::Postgres(url) => vec!["postgres".into(), "--url".into(), url.into()],
            Self::Sqlite(path) => vec!["sqlite".into(), "--path".into(), path.into()],
        }
    }
}

pub(crate) async fn load(
    env: &Environment,
    explicit: Option<PathBuf>,
) -> Result<config::Loaded, Error> {
    let env = env.clone();
    tokio::task::spawn_blocking(move || {
        let location = match explicit {
            Some(path) => ConfigLocation::Current(path),
            None => config::resolve_config_path(&env)?.ok_or(Error::NoConfig)?,
        };
        Ok(Config::load(&env, &location, Profile::Standalone)?)
    })
    .await?
}

pub(crate) async fn run(
    args: DbArgs,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Report, Error> {
    if shutdown.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let mut stderr = String::new();
    let target = match (args.url, args.path) {
        (Some(url), None) => Target::Postgres(url),
        (None, Some(path)) => Target::Sqlite(path),
        (None, None) => {
            let loaded = load(env, args.config).await?;
            stderr = configuration::notices(&loaded);
            Target::configured(&loaded.config, env)?
        }
        (Some(_), Some(_)) => return Err(Error::MemoryBackend),
    };
    if shutdown.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let (label, status, foreign) = match target {
        Target::Postgres(url) => {
            let status = match args.command {
                DbCommand::Migrate => migrate::postgres(&url).await.map_err(Error::Migrate)?,
                DbCommand::Status => migrate::postgres_status(&url)
                    .await
                    .map_err(Error::Migrate)?,
            };
            ("postgres".to_owned(), status, String::new())
        }
        Target::Sqlite(path) => {
            let label = format!("sqlite {}", path.display());
            let foreign_path = path.clone();
            let status = tokio::task::spawn_blocking(move || match args.command {
                DbCommand::Migrate => migrate::sqlite(&path),
                DbCommand::Status => migrate::sqlite_status(&path),
            })
            .await?
            .map_err(Error::Migrate)?;
            let foreign = match args.command {
                DbCommand::Status => sqlite_foreign_line(&foreign_path).await?,
                DbCommand::Migrate => String::new(),
            };
            (label, status, foreign)
        }
    };
    if shutdown.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let state = format!(
        "current {}, required {}, pending {}",
        status.current, status.required, status.pending
    );
    let stdout = match args.command {
        DbCommand::Migrate => format!("{label}: applied; {state}\n"),
        DbCommand::Status if foreign.is_empty() => format!("{label}: {state}\n"),
        DbCommand::Status => format!("{label}: {state}\n{foreign}\n"),
    };
    let mut report = Report::success(stdout);
    report.stderr = stderr;
    Ok(report)
}

async fn sqlite_foreign_line(path: &std::path::Path) -> Result<String, Error> {
    let path = path.to_path_buf();
    let objects = tokio::task::spawn_blocking(move || migrate::sqlite_foreign(&path))
        .await?
        .map_err(Error::Migrate)?;
    Ok(migrate::foreign_line(&objects))
}
