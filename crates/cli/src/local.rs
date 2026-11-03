mod ensure;

use std::io::Write;
use std::path::{Path, PathBuf};

use config::model::GatewaySection;
use config::{Config, ConfigLocation, Environment};

use crate::Error;

pub(crate) use ensure::{Ensure, ensure};

const DEFAULT_GATEWAY_PORT: u16 = 5174;
const LOOPBACK_HOST: &str = "127.0.0.1";
const LAN_HOST: &str = "0.0.0.0";

#[derive(Debug)]
pub struct Prepared {
    pub config: Config,
    pub claim: config::NodeClaim,
    pub database: PathBuf,
}

impl Prepared {
    pub fn release(self) -> Result<(), Error> {
        Ok(self.claim.release()?)
    }
}

pub(crate) async fn prepare(
    env: &Environment,
    port: Option<u16>,
    no_lan: bool,
) -> Result<Prepared, Error> {
    if port == Some(0) {
        return Err(Error::GatewayPort);
    }
    let owned = env.clone();
    tokio::task::spawn_blocking(move || prepare_blocking(&owned, port, no_lan)).await?
}

fn prepare_blocking(env: &Environment, port: Option<u16>, no_lan: bool) -> Result<Prepared, Error> {
    if let Some(notice) = config::adopt_legacy_state(env)? {
        tracing::warn!("{notice}");
    }
    let found = config::resolve_config_path(env)?;
    let state = config::Paths::new(env)?.state_dir();
    let claim = config::claim_node(&state, std::process::id())?;
    let guard = ClaimGuard { claim: Some(claim) };
    let location = match found {
        Some(location) => location,
        None => write_generated(env, &state, port, no_lan)?,
    };
    let mut loaded = config::Config::load(env, &location, config::Profile::Standalone)?;
    for finding in &loaded.warnings {
        tracing::warn!("[{}] {}", finding.path, finding.message);
    }
    for notice in &loaded.notices {
        tracing::warn!("{notice}");
    }
    apply_bind(&mut loaded.config, port, no_lan);
    refuse_plain_lan(&loaded.config, env, location.path())?;
    let database = sqlite_file(&loaded.config, env)?;
    migrate::sqlite(&database).map_err(Error::Migrate)?;
    let claim = guard.into_claim()?;
    Ok(Prepared {
        config: loaded.config,
        claim,
        database,
    })
}

fn write_generated(
    env: &Environment,
    state: &Path,
    port: Option<u16>,
    no_lan: bool,
) -> Result<ConfigLocation, Error> {
    let database = state.join("memory.sqlite");
    let host = bind_host(env, no_lan)?;
    if host == LOOPBACK_HOST && !no_lan {
        tracing::info!(
            "serving on {LOOPBACK_HOST} only: a LAN bind needs TLS (set gateway.host to {LAN_HOST} with gateway.tls_cert and gateway.tls_key)"
        );
    }
    let yaml = config::standalone_config_yaml(&database, host, bind_port(port))?;
    let destination = state.join("config.yaml");
    refuse_existing(&destination)?;
    let partial = state.join(".config.yaml.partial");
    publish(&partial, &destination, yaml.as_bytes())?;
    Ok(ConfigLocation::Current(destination))
}

fn refuse_existing(destination: &Path) -> Result<(), Error> {
    match std::fs::symlink_metadata(destination) {
        Ok(_) => Err(Error::ConfigPresent {
            path: destination.to_path_buf(),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(source.into()),
    }
}

fn publish(partial: &Path, destination: &Path, bytes: &[u8]) -> Result<(), Error> {
    match std::fs::remove_file(partial) {
        Ok(()) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(source.into()),
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(partial)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(partial, destination)?;
    Ok(())
}

fn apply_bind(config: &mut Config, port: Option<u16>, no_lan: bool) {
    if port.is_none() && !no_lan {
        return;
    }
    let section = config.gateway.get_or_insert_with(GatewaySection::default);
    if let Some(port) = port {
        section.port = port;
    } else if section.port == 0 {
        section.port = DEFAULT_GATEWAY_PORT;
    }
    if no_lan {
        section.host = Some(LOOPBACK_HOST.to_owned());
    }
}

fn sqlite_file(config: &Config, env: &Environment) -> Result<PathBuf, Error> {
    if config.postgres_selected(env) {
        return Err(Error::PostgresBackend);
    }
    let Some(path) = config
        .memory
        .as_ref()
        .and_then(|memory| memory.sqlite.as_ref())
        .and_then(|sqlite| sqlite.path.clone())
    else {
        return if config.pglite_import_pending() {
            Ok(config::Paths::new(env)?.state_dir().join("memory.sqlite"))
        } else {
            Err(Error::LocalDatabase)
        };
    };
    if path.as_os_str().is_empty() || path == Path::new(":memory:") {
        return Err(Error::LocalDatabase);
    }
    Ok(path)
}

fn bind_host(env: &Environment, no_lan: bool) -> Result<&'static str, Error> {
    if !no_lan && tls_ready(&Config::default(), env, LAN_HOST)? {
        Ok(LAN_HOST)
    } else {
        Ok(LOOPBACK_HOST)
    }
}

fn tls_ready(config: &Config, env: &Environment, host: &str) -> Result<bool, Error> {
    let hostname = nix::unistd::gethostname()
        .map_err(|_| Error::HostName)?
        .into_string()
        .map_err(|_| Error::HostName)?;
    let node = config::resolve_node_name(config, env, hostname.trim())?;
    let mut section = config.gateway.clone().unwrap_or_default();
    section.host = Some(host.to_owned());
    let source = gateway::server::BindSource::resolve_section(
        Some(&section),
        env,
        &node,
        &config::shared_dir(env),
    );
    Ok(!matches!(
        gateway::tls::resolve_tls(&source),
        Err(gateway::server::BindError::TlsRequired { .. })
    ))
}

fn refuse_plain_lan(config: &Config, env: &Environment, path: &Path) -> Result<(), Error> {
    let host = config
        .gateway
        .as_ref()
        .and_then(|section| section.host.clone())
        .unwrap_or_else(|| LOOPBACK_HOST.to_owned());
    if gateway::server::is_bind_loopback(&host) || tls_ready(config, env, &host)? {
        Ok(())
    } else {
        Err(Error::LanNeedsTls {
            path: path.to_path_buf(),
            host,
        })
    }
}

fn bind_port(port: Option<u16>) -> u16 {
    match port {
        Some(port) => port,
        None => DEFAULT_GATEWAY_PORT,
    }
}

struct ClaimGuard {
    claim: Option<config::NodeClaim>,
}

impl ClaimGuard {
    fn into_claim(mut self) -> Result<config::NodeClaim, Error> {
        self.claim.take().ok_or(Error::ClaimMissing)
    }
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        if let Some(claim) = self.claim.take() {
            match claim.release() {
                Ok(()) => {}
                Err(error) => tracing::error!("pid file removal failed: {error}"),
            }
        }
    }
}
