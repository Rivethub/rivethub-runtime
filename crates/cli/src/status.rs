use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use config::{Config, Environment};
use gateway::server::BindSource;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use serde::Deserialize;
use wire::gateway::HealthzResponse;
use x509_parser::extensions::GeneralName;

use crate::{Error, Report, configuration, process};

#[derive(Debug, Deserialize)]
struct Health {
    #[serde(flatten)]
    response: HealthzResponse,
    uptime: Option<f64>,
}

pub(crate) async fn run(env: &Environment) -> Report {
    match health(env).await {
        Ok(report) => report,
        Err(error) => {
            let mut report = match process::status(env).await {
                Ok(message) => Report::success(message),
                Err(pid_error) => {
                    let mut report = Report::success("RivetHub status is unavailable.\n");
                    report.stderr = format!("Error: {pid_error}\n");
                    report
                }
            };
            report.stderr = format!("{}Health unavailable: {error}\n", report.stderr);
            if matches!(&error, Error::Http(source) if source.status() == Some(reqwest::StatusCode::UNAUTHORIZED))
            {
                report
                    .stderr
                    .push_str("The gateway wants a login. Run `rh open` to sign in.\n");
            }
            report
        }
    }
}

pub(crate) struct Endpoint {
    pub(crate) url: url::Url,
    pub(crate) client: reqwest::Client,
}

async fn health(env: &Environment) -> Result<Report, Error> {
    let (_, loaded) = configuration::load(env).await?;
    let Endpoint { url, client } = endpoint(env, &loaded.config).await?;
    let health: Health = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let text = if health.response.ok {
        "RivetHub is running".to_owned()
    } else {
        "RivetHub is not running".to_owned()
    };
    let mut parts = vec![text];
    if let Some(version) = health.response.version {
        parts.push(format!(" (v{version})"));
    }
    if let Some(uptime) = health.uptime {
        parts.push(format!("; uptime {uptime}s"));
    }
    parts.push("\n".to_owned());
    for provider in &health.response.degraded_providers {
        parts.push(format!(
            "Provider {} is degraded ({}): {}\n",
            provider.id, provider.recovery, provider.reason
        ));
    }
    let mut report = Report::success(parts.join(""));
    report.stderr = configuration::notices(&loaded);
    Ok(report)
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Redirects {
    Follow,
    Refuse,
}

pub(crate) async fn endpoint(env: &Environment, config: &Config) -> Result<Endpoint, Error> {
    health_endpoint(env, config, Redirects::Follow).await
}

pub(crate) async fn probe_endpoint(env: &Environment, config: &Config) -> Result<Endpoint, Error> {
    health_endpoint(env, config, Redirects::Refuse).await
}

async fn health_endpoint(
    env: &Environment,
    config: &Config,
    redirects: Redirects,
) -> Result<Endpoint, Error> {
    let Endpoint { url, client } =
        gateway_with(env, config, Duration::from_secs(3), redirects).await?;
    Ok(Endpoint {
        url: url.join("healthz")?,
        client,
    })
}

pub(crate) async fn gateway(
    env: &Environment,
    config: &Config,
    timeout: Duration,
) -> Result<Endpoint, Error> {
    gateway_with(env, config, timeout, Redirects::Follow).await
}

async fn gateway_with(
    env: &Environment,
    config: &Config,
    timeout: Duration,
    redirects: Redirects,
) -> Result<Endpoint, Error> {
    let hostname = host_name()?;
    let node = config::resolve_node_name(config, env, &hostname)?;
    let section = config.gateway.clone();
    let tls_env = env.clone();
    let shared = config::shared_dir(&tls_env);
    let source = tokio::task::spawn_blocking(move || {
        BindSource::resolve_section(section.as_ref(), &tls_env, &node, &shared)
    })
    .await?;
    let mut url = url::Url::parse(if source.tls_cert.is_some() {
        "https://localhost/"
    } else {
        "http://localhost/"
    })?;
    let host = if source.host.contains(':') && !source.host.starts_with('[') {
        format!("[{}]", source.host)
    } else {
        source.host
    };
    url.set_host(Some(&host))?;
    match url.host() {
        Some(url::Host::Ipv4(address)) if address.is_unspecified() => {
            url.set_host(Some("127.0.0.1"))?;
        }
        Some(url::Host::Ipv6(address)) if address.is_unspecified() => {
            url.set_host(Some("[::1]"))?;
        }
        Some(url::Host::Domain(_) | url::Host::Ipv4(_) | url::Host::Ipv6(_)) => {}
        None => return Err(Error::Address),
    }
    url.set_port(Some(source.port))
        .map_err(|()| Error::Address)?;
    let policy = match redirects {
        Redirects::Follow => reqwest::redirect::Policy::default(),
        Redirects::Refuse => reqwest::redirect::Policy::none(),
    };
    let builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(policy)
        .timeout(timeout);
    let builder = match gateway::local_owner::read_token(&gateway::state_dir(env)?)? {
        Some(token) => builder.default_headers(owner_header(token.expose())?),
        None => builder,
    };
    let ca = gateway::private_ca(env, &config::shared_dir(env));
    let client = match (&source.tls_cert, &ca) {
        (Some(cert), Some(ca)) => verified(builder, &mut url, cert, ca, source.port)?,
        _ => builder,
    }
    .build()?;
    Ok(Endpoint { url, client })
}

fn owner_header(token: &str) -> Result<reqwest::header::HeaderMap, Error> {
    let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| Error::CaptureToken)?;
    value.set_sensitive(true);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::AUTHORIZATION, value);
    Ok(headers)
}

fn verified(
    builder: reqwest::ClientBuilder,
    url: &mut url::Url,
    cert: &Path,
    ca: &Path,
    port: u16,
) -> Result<reqwest::ClientBuilder, Error> {
    let roots = reqwest::Certificate::from_pem_bundle(&std::fs::read(ca)?)?;
    let builder = builder.tls_certs_only(roots);
    let Some(name) = served_name(cert)? else {
        return Ok(builder);
    };
    let address = match url.host() {
        Some(url::Host::Ipv4(address)) => IpAddr::V4(address),
        Some(url::Host::Ipv6(address)) => IpAddr::V6(address),
        Some(url::Host::Domain(_)) | None => return Ok(builder),
    };
    url.set_host(Some(&name))?;
    Ok(builder.resolve(&name, SocketAddr::new(address, port)))
}

fn served_name(cert: &Path) -> Result<Option<String>, Error> {
    let der = CertificateDer::from_pem_file(cert)
        .map_err(|_| Error::GatewayCertificate("gateway certificate is not a PEM certificate"))?;
    let (_, parsed) = x509_parser::parse_x509_certificate(&der)
        .map_err(|_| Error::GatewayCertificate("gateway certificate does not parse"))?;
    let names = parsed
        .subject_alternative_name()
        .map_err(|_| Error::GatewayCertificate("invalid gateway certificate SAN"))?;
    Ok(names.and_then(|san| {
        san.value
            .general_names
            .iter()
            .find_map(|general| match general {
                GeneralName::DNSName(name) => Some((*name).to_owned()),
                GeneralName::OtherName(_, _)
                | GeneralName::RFC822Name(_)
                | GeneralName::X400Address(_)
                | GeneralName::DirectoryName(_)
                | GeneralName::EDIPartyName(_)
                | GeneralName::URI(_)
                | GeneralName::IPAddress(_)
                | GeneralName::RegisteredID(_)
                | GeneralName::Invalid(_, _) => None,
            })
    }))
}

fn host_name() -> Result<String, Error> {
    let raw = nix::unistd::gethostname().map_err(|_| Error::HostName)?;
    let text = raw.into_string().map_err(|_| Error::HostName)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        Ok(String::new())
    } else {
        Ok(trimmed.to_owned())
    }
}
