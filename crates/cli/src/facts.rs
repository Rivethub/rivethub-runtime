use std::path::{Path, PathBuf};

use clap::Subcommand;
use config::{Config, Environment};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use semver::Version;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::gateway_target::{self, GatewayTarget};
use crate::status;
use crate::{Error, Report, configuration};

#[derive(Debug, Subcommand)]
pub enum TlsCommand {
    #[command(about = "Print the fingerprints of the certificate this node serves")]
    Fingerprint {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Serialize)]
struct NodeStatus {
    running: bool,
    version: Option<String>,
    cli_version: &'static str,
    origin: String,
    health_error: Option<String>,
    web: Web,
    tls: Option<Tls>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum WebSource {
    None,
    Managed,
    Explicit,
}

#[derive(Debug, Serialize)]
struct Web {
    installed: bool,
    source: WebSource,
    path: Option<PathBuf>,
    version: Option<Version>,
    min_rh_version: Option<Version>,
    compatible: Option<bool>,
}

#[derive(Debug, Serialize)]
struct Tls {
    cert: PathBuf,
    ca: Option<PathBuf>,
    sha256: String,
    spki_sha256: String,
    not_after: jiff::Timestamp,
}

pub(crate) fn origin(url: &url::Url) -> String {
    url.origin().ascii_serialization()
}

pub(crate) async fn status(env: &Environment, json: bool) -> Result<Report, Error> {
    if !json {
        return Ok(status::run(env).await);
    }
    let (_, loaded) = configuration::load(env).await?;
    let target = gateway_target::gateway_target(env, &loaded.config).await?;
    let (health, health_error) = match status::probe(env, &loaded.config).await {
        Ok(health) => (Some(health), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let version = health.as_ref().and_then(|health| health.version.clone());
    let runtime = if let Some(version) = &version {
        version
    } else {
        env!("CARGO_PKG_VERSION")
    };
    let runtime = Version::parse(runtime)
        .map_err(|_| crate::update::UpdateError::Version(runtime.to_owned()))?;
    let report = NodeStatus {
        running: health.is_some_and(|health| health.ok),
        cli_version: env!("CARGO_PKG_VERSION"),
        origin: origin(&target.url),
        web: web(env, &loaded.config, &runtime)?,
        tls: tls(&target)?,
        health_error,
        version,
    };
    machine(&report)
}

pub(crate) async fn tls_command(command: TlsCommand, env: &Environment) -> Result<Report, Error> {
    let TlsCommand::Fingerprint { json } = command;
    let (_, loaded) = configuration::load(env).await?;
    let target = gateway_target::gateway_target(env, &loaded.config).await?;
    let Some(tls) = tls(&target)? else {
        return Err(Error::NoTls {
            origin: origin(&target.url),
        });
    };
    if json {
        return machine(&tls);
    }
    Ok(Report::success(format!(
        "SHA-256       {}\nSPKI SHA-256  {}\nCertificate   {}\nCA            {}\nExpires       {}\n",
        tls.sha256,
        tls.spki_sha256,
        tls.cert.display(),
        tls.ca
            .as_deref()
            .map_or_else(|| "none".to_owned(), |ca| ca.display().to_string()),
        tls.not_after,
    )))
}

pub(crate) fn machine(document: &impl Serialize) -> Result<Report, Error> {
    Ok(Report::success(format!(
        "{}\n",
        serde_json::to_string(document)?
    )))
}

fn web(env: &Environment, config: &Config, runtime: &Version) -> Result<Web, Error> {
    if let Some(path) = gateway::explicit_static_dir(config, env) {
        return Ok(Web {
            installed: path.join("index.html").is_file(),
            source: WebSource::Explicit,
            path: Some(path),
            version: None,
            min_rh_version: None,
            compatible: None,
        });
    }
    let Some(active) = crate::update::active_web(&gateway::state_dir(env)?)? else {
        return Ok(Web {
            installed: false,
            source: WebSource::None,
            path: None,
            version: None,
            min_rh_version: None,
            compatible: None,
        });
    };
    let (version, min_rh_version) = match active.build {
        Some(build) => (Some(build.version), Some(build.min_rh_version)),
        None => (None, None),
    };
    Ok(Web {
        installed: true,
        source: WebSource::Managed,
        compatible: min_rh_version
            .as_ref()
            .map(|minimum| minimum.cmp_precedence(runtime).is_le()),
        path: Some(active.path),
        version,
        min_rh_version,
    })
}

fn tls(target: &GatewayTarget) -> Result<Option<Tls>, Error> {
    let Some(cert) = &target.source.tls_cert else {
        return Ok(None);
    };
    let (sha256, spki_sha256, not_after) = fingerprints(cert)?;
    Ok(Some(Tls {
        cert: cert.clone(),
        ca: target.ca.clone(),
        sha256,
        spki_sha256,
        not_after,
    }))
}

fn fingerprints(cert: &Path) -> Result<(String, String, jiff::Timestamp), Error> {
    const UNREADABLE: Error =
        Error::GatewayCertificate("gateway certificate is not a PEM certificate");
    let der = CertificateDer::from_pem_file(cert).map_err(|_| UNREADABLE)?;
    let (_, parsed) = x509_parser::parse_x509_certificate(&der).map_err(|_| UNREADABLE)?;
    let not_after = jiff::Timestamp::from_second(parsed.validity().not_after.timestamp())
        .map_err(|_| UNREADABLE)?;
    Ok((
        hex::encode(Sha256::digest(&der)),
        hex::encode(Sha256::digest(parsed.public_key().raw)),
        not_after,
    ))
}
