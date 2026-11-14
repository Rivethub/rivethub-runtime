use gateway::account::AccountLogin as _;
mod cloud;
mod error;
mod files;
mod node;
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
use node::Restart;
use release::Staged;

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
    #[arg(long, value_name = "rivethub-cloud|github")]
    pub source: Option<wire::gateway::account::UpdateSource>,
}

#[derive(Debug)]
pub struct Updater {
    binary: PathBuf,
    releases: Url,
    scope: Scope,
    running: String,
}

#[derive(Debug)]
struct Prepared {
    installation: Arc<files::Installation>,
    config: config::Config,
    target: db::Target,
    restart: Restart,
}

impl Updater {
    pub fn new(binary: PathBuf, releases: Url, scope: Scope) -> Self {
        Self {
            binary,
            releases,
            scope,
            running: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    #[must_use]
    pub fn running(mut self, version: &str) -> Self {
        version.clone_into(&mut self.running);
        self
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
        let unit = self.scope.unit_path(config::Paths::new(env)?.home());
        let restart = Restart::plan(&unit, args.no_restart).await?;
        let binary = self.binary.clone();
        let installation = Arc::new(
            tokio::task::spawn_blocking(move || files::Installation::prepare(binary)).await??,
        );
        Ok(Prepared {
            installation,
            config: loaded.config,
            target,
            restart,
        })
    }

    async fn source(
        &self,
        args: &UpdateArgs,
        env: &Environment,
        config: &config::Config,
    ) -> Result<UpdateSource, UpdateError> {
        if config.cloud_disabled() {
            return match args.source {
                Some(UpdateSource::RivethubCloud) => {
                    Err(gateway::account::AccountError::CloudPolicy.into())
                }
                Some(UpdateSource::Github) | None => Ok(UpdateSource::Github),
            };
        }
        if let Some(source) = args.source {
            return Ok(source);
        }
        let files = gateway::account::AccountFiles::new(&gateway::state_dir(env)?);
        let state = tokio::task::spawn_blocking(move || files.read()).await??;
        let settings = gateway::account::CloudSettings::from_config(config)?;
        let login = crate::account_login::NodeAccountLogin::new(env.clone(), &settings.base);
        Ok(
            if login
                .identity()
                .await
                .map_err(gateway::account::AccountError::from)?
                .is_some()
            {
                state
                    .and_then(|state| state.update_source)
                    .unwrap_or(UpdateSource::RivethubCloud)
            } else {
                UpdateSource::Github
            },
        )
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
            restart,
        } = self.prepare(args, env, shutdown, report).await?;
        let result = async {
            report
                .stdout
                .extend(["Updating this node (", &args.channel.to_string(), ")...\n"]);
            let directory = installation.staging.path();
            let staged = match (args.channel, self.source(args, env, &config).await?) {
                (Channel::Stable | Channel::Beta, UpdateSource::RivethubCloud) => {
                    report
                        .stdout
                        .push_str("Fetching the signed release from rivethub.cloud...\n");
                    Staged::Ready(cloud::stage(&config, env, args, directory, shutdown).await?)
                }
                (Channel::Stable | Channel::Beta, UpdateSource::Github) => {
                    release::stage(
                        &self.releases,
                        args.version.as_deref(),
                        args.channel == Channel::Beta,
                        &self.running,
                        directory,
                        shutdown,
                    )
                    .await?
                }
                (Channel::Nightly, _) => {
                    Staged::Ready(source::stage(args, env, directory, shutdown).await?)
                }
            };
            let version = match staged.version(&self.running, args.channel) {
                Ok(version) => version,
                Err(nothing) => {
                    report.stdout.push_str(&nothing);
                    return Ok(());
                }
            };
            check_cancelled(shutdown)?;
            migrate(directory, &target, env, shutdown, report).await?;
            check_cancelled(shutdown)?;
            let installing = Arc::clone(&installation);
            tokio::task::spawn_blocking(move || installing.install()).await??;
            report.stdout.extend([
                "Installed ",
                &version,
                "; previous binaries saved with .previous suffix.\n",
            ]);
            match restart {
                Restart::Skipped => report.stdout.push_str("Restart skipped (--no-restart).\n"),
                Restart::Service => self.restart(env, shutdown, report).await?,
                Restart::Standalone => {
                    node::restart_standalone(&self.binary, env, shutdown, report).await?;
                }
            }
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

async fn migrate(
    staging: &std::path::Path,
    target: &db::Target,
    env: &Environment,
    shutdown: &CancellationToken,
    report: &mut Report,
) -> Result<(), Error> {
    report.stdout.push_str("Applying database migrations...\n");
    let mut migration = tokio::process::Command::new(staging.join("rivethub-migrate"));
    migration
        .env_clear()
        .envs(env.iter())
        .kill_on_drop(true)
        .args(target.arguments());
    let migrated = require_success(capture(migration, "rivethub-migrate", shutdown).await?)?;
    report.stdout.push_str(&migrated.stdout);
    report.stderr.push_str(&migrated.stderr);
    Ok(())
}

fn check_cancelled(shutdown: &CancellationToken) -> Result<(), UpdateError> {
    if shutdown.is_cancelled() {
        Err(UpdateError::Cancelled)
    } else {
        Ok(())
    }
}
