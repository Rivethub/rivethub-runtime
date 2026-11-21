use gateway::account::AccountLogin as _;
mod cloud;
mod error;
mod files;
mod node;
mod release;
mod source;
mod web;

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
pub use web::{Active as ActiveWeb, active as active_web};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    ValueEnum,
    strum::Display,
    serde::Deserialize,
    serde::Serialize,
)]
#[serde(rename_all = "lowercase")]
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
    #[arg(long, conflicts_with_all = ["version", "no_restart"], help = "Install only the web app")]
    pub web_only: bool,
    #[arg(long, value_name = "rivethub-cloud|github")]
    pub source: Option<wire::gateway::account::UpdateSource>,
    #[arg(
        long,
        value_name = "ARCHIVE",
        requires_all = ["web_only", "sha256"],
        conflicts_with = "source",
        help = "Install the web app from this archive instead of a published release"
    )]
    pub from: Option<PathBuf>,
    #[arg(
        long,
        value_name = "HEX",
        requires = "from",
        help = "The archive's SHA-256"
    )]
    pub sha256: Option<String>,
    #[arg(
        long,
        value_name = "FILE",
        requires = "from",
        help = "The archive's web-manifest.json [default: beside the archive]"
    )]
    pub manifest: Option<PathBuf>,
}

#[derive(Debug)]
pub struct Updater {
    binary: PathBuf,
    releases: Url,
    web_releases: Url,
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
    pub fn new(binary: PathBuf, releases: Url, web_releases: Url, scope: Scope) -> Self {
        Self {
            binary,
            releases,
            web_releases,
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
        let web_store = prepare_web(env, shutdown).await;
        let mut runtime = self.running.clone();
        if !args.web_only
            && !matches!(&web_store, Err(Error::Update(UpdateError::Busy)))
            && let Err(error) = self
                .execute(args, env, shutdown, &mut report, &mut runtime)
                .await
        {
            report.stderr.extend(["Error: ", &error.to_string(), "\n"]);
            report.exit = Exit::Failure;
        }
        let outcome = match (web_store, web::Local::parse(args)) {
            (_, Err(error)) => web::Outcome::Refused(error.to_string()),
            (Ok(Some(store)), Ok(Some(local))) => {
                match tokio::task::spawn_blocking(move || store.install_local(&local, &runtime))
                    .await
                    .map_err(UpdateError::from)
                    .and_then(|installed| installed)
                {
                    Ok(outcome) => outcome,
                    Err(error) => web::Outcome::Refused(error.to_string()),
                }
            }
            (Ok(None), Ok(Some(_))) => web::Outcome::Refused(UpdateError::WebExplicit.to_string()),
            (Err(error), Ok(Some(_))) => web::Outcome::Refused(error.to_string()),
            (Ok(Some(store)), Ok(None)) => {
                match store
                    .refresh(&self.web_releases, args.channel, &runtime, shutdown)
                    .await
                {
                    Ok(outcome) => outcome,
                    Err(error) => web::Outcome::Failed(error.to_string()),
                }
            }
            (Ok(None), Ok(None)) => web::Outcome::Explicit,
            (Err(error), Ok(None)) => web::Outcome::Failed(error.to_string()),
        };
        outcome.report(args.channel, &mut report);
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
        if let db::Target::Postgres(url) = &target {
            memory_postgres::PostgresMemory::check_serving_url(url)
                .await
                .map_err(|refused| UpdateError::ServingLogin(refused.to_string()))?;
        }
        let target = match (target, env.text(MIGRATION_URL)) {
            (db::Target::Postgres(_), Some(owner)) => db::Target::Postgres(owner.to_owned()),
            (target, _) => target,
        };
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
        if args.version.is_some() {
            return Ok(UpdateSource::Github);
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

    async fn stage_github(
        &self,
        args: &UpdateArgs,
        directory: &std::path::Path,
        shutdown: &CancellationToken,
    ) -> Result<Staged, UpdateError> {
        release::stage(
            &self.releases,
            args.version.as_deref(),
            args.channel == Channel::Beta,
            &self.running,
            directory,
            shutdown,
        )
        .await
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

    async fn restart_installed(
        &self,
        restart: Restart,
        env: &Environment,
        shutdown: &CancellationToken,
        report: &mut Report,
    ) -> Result<(), Error> {
        match restart {
            Restart::Skipped => {
                let note = self.scope.repair_preface(env).await?;
                if note.starts_with("Updated ") {
                    self.scope.reload(env, shutdown).await?;
                }
                report.stdout.push_str(&note);
                report.stdout.push_str("Restart skipped (--no-restart).\n");
            }
            Restart::Service => self.restart(env, shutdown, report).await?,
            Restart::Standalone => {
                node::restart_standalone(&self.binary, env, shutdown, report).await?;
            }
        }
        Ok(())
    }

    async fn execute(
        &self,
        args: &UpdateArgs,
        env: &Environment,
        shutdown: &CancellationToken,
        report: &mut Report,
        runtime: &mut String,
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
                    match cloud::stage(&config, env, args, directory, shutdown).await {
                        Ok(version) => Staged::Ready(version),
                        Err(UpdateError::NoCloudRelease(channel)) if args.source.is_none() => {
                            report.stdout.extend([
                                "rivethub.cloud has no ",
                                &channel.to_string(),
                                " release for this platform; using the GitHub release instead.\n",
                            ]);
                            self.stage_github(args, directory, shutdown).await?
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                (Channel::Stable | Channel::Beta, UpdateSource::Github) => {
                    self.stage_github(args, directory, shutdown).await?
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
            let installed_version = if args.channel == Channel::Nightly {
                source::runtime_version(directory, env, shutdown).await?
            } else {
                version.strip_prefix('v').unwrap_or(&version).to_owned()
            };
            check_cancelled(shutdown)?;
            migrate(directory, &target, env, shutdown, report).await?;
            check_cancelled(shutdown)?;
            let installing = Arc::clone(&installation);
            tokio::task::spawn_blocking(move || installing.install()).await??;
            *runtime = installed_version;
            report.stdout.extend([
                "Installed ",
                &version,
                "; previous binaries saved with .previous suffix.\n",
            ]);
            self.restart_installed(restart, env, shutdown, report)
                .await?;
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

async fn prepare_web(
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Option<web::Store>, Error> {
    check_cancelled(shutdown)?;
    let env = env.clone();
    tokio::task::spawn_blocking(move || {
        let config = match config::resolve_config_path(&env)? {
            Some(path) => config::Config::load(&env, &path)?.config,
            None => config::Config::default(),
        };
        if gateway::explicit_static_dir(&config, &env).is_some() {
            return Ok(None);
        }
        let state = gateway::state_dir(&env)?;
        let paths = config::Paths::new(&env)?;
        if env.text("RIVETHUB_HOME").is_none()
            && paths.legacy_home().symlink_metadata().is_ok()
            && !paths.state_dir().try_exists()?
            && state.parent() == Some(paths.home().canonicalize()?.as_path())
            && state.file_name() == paths.state_dir().file_name()
        {
            return Err(UpdateError::WebLegacyState.into());
        }
        Ok(Some(web::Store::open(&state)?))
    })
    .await?
}

fn web_endpoint(
    #[cfg_attr(not(debug_assertions), expect(unused_variables))] env: &Environment,
) -> Result<Url, Error> {
    #[cfg(debug_assertions)]
    if let Some(endpoint) = env.text("RIVETHUB_TEST_WEB_RELEASES") {
        let url = Url::parse(endpoint)?;
        if url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "[::1]")) {
            return Ok(url);
        }
    }
    Ok(Url::parse(web::RELEASES)?)
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
        web_endpoint(env)?,
        Scope::for_uid(nix::unistd::geteuid().as_raw()),
    );
    Ok(updater.run(&args, env, shutdown).await)
}

const MIGRATION_URL: &str = "RIVETHUB_MIGRATION_URL";

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
