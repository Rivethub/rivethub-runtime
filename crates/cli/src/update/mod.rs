mod cloud;
mod error;
mod files;
mod mesh;
mod release;
mod source;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, ValueEnum};
use config::Environment;
use tokio_util::sync::CancellationToken;
use url::Url;
use wire::gateway::account::UpdateSource;

use crate::service::command::{capture, require_success};
use crate::service::{Scope, ServiceCommand};
use crate::{Error, Exit, Report, configuration, db};
pub use error::UpdateError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, strum::Display)]
#[strum(serialize_all = "lowercase")]
pub enum Channel {
    Stable,
    Beta,
    Nightly,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    #[arg(long, value_enum, default_value = "stable")]
    pub channel: Channel,
    #[arg(long, value_name = "TAG")]
    pub version: Option<String>,
    #[arg(long)]
    pub no_restart: bool,
    #[arg(long)]
    pub mesh: bool,
    #[arg(long, requires = "mesh")]
    pub ssh_user: Option<String>,
    #[arg(long, value_name = "rivethub-cloud|github")]
    pub source: Option<wire::gateway::account::UpdateSource>,
}

#[derive(Debug)]
pub struct Updater {
    binary: PathBuf,
    releases: Url,
    scope: Scope,
}

#[derive(Debug)]
struct Prepared {
    installation: Arc<files::Installation>,
    config: config::Config,
    target: db::Target,
}

impl Updater {
    pub fn new(binary: PathBuf, releases: Url, scope: Scope) -> Self {
        Self {
            binary,
            releases,
            scope,
        }
    }

    pub async fn run(
        &self,
        args: &UpdateArgs,
        env: &Environment,
        shutdown: &CancellationToken,
    ) -> Report {
        let mut report = Report::success("");
        if let Err(error) = self.execute(args, env, shutdown, &mut report).await {
            report.stderr.extend(["Error: ", &error.to_string(), "\n"]);
            report.exit = Exit::Failure;
        }
        report
    }

    async fn prepare(
        &self,
        args: &UpdateArgs,
        env: &Environment,
        shutdown: &CancellationToken,
        report: &mut Report,
    ) -> Result<Prepared, Error> {
        check_cancelled(shutdown)?;
        release::platform()?;
        if let Some(version) = &args.version {
            release::validate_version(version)?;
        }
        let loaded = db::load(env, None).await?;
        report.stderr.push_str(&configuration::notices(&loaded));
        let target = db::Target::configured(&loaded.config, env)?;
        if !args.no_restart {
            let paths = config::Paths::new(env)?;
            let unit = self.scope.unit_path(paths.home());
            if !tokio::fs::metadata(&unit).await?.is_file() {
                return Err(UpdateError::Path(unit).into());
            }
        }
        let binary = self.binary.clone();
        let installation = Arc::new(
            tokio::task::spawn_blocking(move || files::Installation::prepare(binary)).await??,
        );
        Ok(Prepared {
            installation,
            config: loaded.config,
            target,
        })
    }

    async fn source(
        &self,
        args: &UpdateArgs,
        env: &Environment,
    ) -> Result<UpdateSource, UpdateError> {
        if let Some(source) = args.source {
            return Ok(source);
        }
        let files = gateway::account::AccountFiles::new(&gateway::state_dir(env)?);
        let state = tokio::task::spawn_blocking(move || files.read()).await??;
        Ok(state.map_or(UpdateSource::Github, |state| state.update_source()))
    }

    async fn restart(
        &self,
        env: &Environment,
        shutdown: &CancellationToken,
        report: &mut Report,
    ) -> Result<(), Error> {
        report.stdout.push_str("Restarting RivetHub service...\n");
        let restarted = self
            .scope
            .run(ServiceCommand::Restart, env, shutdown)
            .await?;
        let restarted = require_success(restarted)?;
        report.stdout.push_str(&restarted.stdout);
        report.stderr.push_str(&restarted.stderr);
        Ok(())
    }

    async fn execute(
        &self,
        args: &UpdateArgs,
        env: &Environment,
        shutdown: &CancellationToken,
        report: &mut Report,
    ) -> Result<(), Error> {
        let Prepared {
            installation,
            config,
            target,
        } = self.prepare(args, env, shutdown, report).await?;
        let result = async {
            let peers = if args.mesh {
                report.stdout.push_str("Checking mesh nodes...\n");
                mesh::preflight(args, &config, env, shutdown, report).await?
            } else {
                Vec::new()
            };
            report
                .stdout
                .extend(["Updating this node (", &args.channel.to_string(), ")...\n"]);
            let version = match (args.channel, self.source(args, env).await?) {
                (Channel::Stable | Channel::Beta, UpdateSource::RivethubCloud) => {
                    report
                        .stdout
                        .push_str("Fetching the signed release from rivethub.cloud...\n");
                    cloud::stage(&config, env, args, installation.staging.path(), shutdown).await?
                }
                (Channel::Stable | Channel::Beta, UpdateSource::Github) => {
                    release::stage(
                        &self.releases,
                        args.version.as_deref(),
                        args.channel == Channel::Beta,
                        installation.staging.path(),
                        shutdown,
                    )
                    .await?
                }
                (Channel::Nightly, _) => {
                    source::stage(args, env, installation.staging.path(), shutdown).await?
                }
            };
            check_cancelled(shutdown)?;
            report.stdout.push_str("Applying database migrations...\n");
            let mut migration =
                tokio::process::Command::new(installation.staging.path().join("rivethub-migrate"));
            migration
                .env_clear()
                .envs(env.iter())
                .kill_on_drop(true)
                .args(target.arguments());
            let migrated =
                require_success(capture(migration, "rivethub-migrate", shutdown).await?)?;
            report.stdout.push_str(&migrated.stdout);
            report.stderr.push_str(&migrated.stderr);
            check_cancelled(shutdown)?;
            let installing = Arc::clone(&installation);
            tokio::task::spawn_blocking(move || installing.install()).await??;
            report.stdout.extend([
                "Installed ",
                &version,
                "; previous binaries saved with .previous suffix.\n",
            ]);
            if args.no_restart {
                report.stdout.push_str("Restart skipped (--no-restart).\n");
            } else {
                self.restart(env, shutdown, report).await?;
            }
            mesh::update(peers, args, &version, env, shutdown, report).await?;
            Ok::<_, Error>(())
        }
        .await;
        tokio::task::spawn_blocking(move || {
            let installation = Arc::try_unwrap(installation)
                .map_err(|_| std::io::Error::other("update staging is still in use"))?;
            installation.staging.close()
        })
        .await??;
        result
    }
}

pub(crate) async fn run(
    args: UpdateArgs,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Report, Error> {
    let binary = tokio::task::spawn_blocking(std::env::current_exe).await??;
    let repository = Url::parse(env!("CARGO_PKG_REPOSITORY"))?;
    let releases = Url::parse(&format!(
        "https://api.github.com/repos{}/releases/",
        repository.path()
    ))?;
    let updater = Updater::new(
        binary,
        releases,
        Scope::for_uid(nix::unistd::geteuid().as_raw()),
    );
    Ok(updater.run(&args, env, shutdown).await)
}

fn check_cancelled(shutdown: &CancellationToken) -> Result<(), UpdateError> {
    if shutdown.is_cancelled() {
        Err(UpdateError::Cancelled)
    } else {
        Ok(())
    }
}
