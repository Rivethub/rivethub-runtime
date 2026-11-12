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
    let mut report = health_report(env).await;
    let line = match crate::capture_hook::spool_warning(env).await {
        Ok(Some(warning)) => warning,
        Ok(None) => String::new(),
        Err(error) => format!("The capture spool could not be read: {error}"),
    };
    if !line.is_empty() {
        report.stderr = format!("{}{line}\n", report.stderr);
    }
    report
}

async fn health_report(env: &Environment) -> Report {
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
    let mut health: Health = fetch(&client, url).await?;
    let uid = nix::unistd::getuid().as_raw();
    let mut notices = configuration::notices(&loaded);
    match detailed_health(env, &loaded.config, uid).await {
        Ok(Some(detailed)) => health = detailed,
        Ok(None) => {}
        Err(error) => {
            notices.push_str("Detailed health unavailable: ");
            notices.push_str(&error.to_string());
            notices.push('\n');
        }
    }
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
    report.stderr = notices;
    Ok(report)
}

async fn fetch(client: &reqwest::Client, url: url::Url) -> Result<Health, Error> {
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

async fn detailed_health(
    env: &Environment,
    config: &Config,
    owner_uid: u32,
) -> Result<Option<Health>, Error> {
    if gateway::local_owner::read_token(&gateway::state_dir(env)?)?.is_none() {
        return Ok(None);
    }
    let Endpoint { url, client } =
        gateway_as(env, config, Duration::from_secs(3), Some(owner_uid)).await?;
    fetch(&client, url.join("healthz")?).await.map(Some)
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
        gateway_with(env, config, Duration::from_secs(3), redirects, None).await?;
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
    gateway_as(env, config, timeout, Some(nix::unistd::getuid().as_raw())).await
}

pub(crate) async fn gateway_as(
    env: &Environment,
    config: &Config,
    timeout: Duration,
    owner_uid: Option<u32>,
) -> Result<Endpoint, Error> {
    gateway_with(env, config, timeout, Redirects::Follow, owner_uid).await
}

async fn gateway_with(
    env: &Environment,
    config: &Config,
    timeout: Duration,
    redirects: Redirects,
    owner_uid: Option<u32>,
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
    let ca = gateway::private_ca(env, &config::shared_dir(env));
    let verified_tls = source.tls_cert.is_some() && ca.is_some();
    let token = match owner_uid {
        Some(_) => gateway::local_owner::read_token(&gateway::state_dir(env)?)?,
        None => None,
    };
    let builder = match (token, owner_uid) {
        (Some(token), Some(uid)) => {
            if !verified_tls {
                crate::listener::require_ours(source.port, uid).await?;
            }
            builder.default_headers(owner_header(token.expose())?)
        }
        _ => builder,
    };
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use config::Environment;

    use super::gateway_as;

    const CONFIG: &str = "runtime:\n  workspace: /home/user/work\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://127.0.0.1:1/v1\n    model: m\n    api_key: secret\n";

    async fn squatted(
        owner_uid: u32,
    ) -> (Result<super::Endpoint, crate::Error>, Vec<Option<String>>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&seen);
        let app = axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
            let recorder = std::sync::Arc::clone(&recorder);
            async move {
                recorder.lock().push(
                    headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned),
                );
                axum::http::StatusCode::OK
            }
        });
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let home = tempfile::tempdir().expect("home");
        let env = Environment::from_vars([("HOME".as_ref(), home.path().as_os_str())]);
        let state = gateway::state_dir(&env).expect("state");
        std::fs::create_dir_all(&state).expect("state dir");
        let token = gateway::local_owner::OwnerToken::generate().expect("token");
        let path = gateway::local_owner::token_path(&state);
        std::fs::write(&path, token.expose()).expect("token file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        std::fs::write(
            home.path().join(".rivethub/config.yaml"),
            format!("{CONFIG}gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
        )
        .expect("config");
        let (_, loaded) = crate::configuration::load(&env).await.expect("config");
        let built = gateway_as(
            &env,
            &loaded.config,
            std::time::Duration::from_secs(3),
            Some(owner_uid),
        )
        .await;
        if let Ok(endpoint) = &built {
            endpoint.client.get(endpoint.url.clone()).send().await.ok();
        }
        let detailed = super::detailed_health(&env, &loaded.config, owner_uid).await;
        assert_eq!(
            matches!(detailed, Err(crate::Error::ListenerNotOurs { .. })),
            owner_uid != nix::unistd::getuid().as_raw()
        );
        server.abort();
        let seen = seen.lock().clone();
        (built, seen)
    }

    #[tokio::test]
    async fn the_token_goes_only_to_a_listener_of_the_current_user() {
        let uid = nix::unistd::getuid().as_raw();
        let (refused, seen) = squatted(uid.wrapping_add(1)).await;
        assert!(
            matches!(refused, Err(crate::Error::ListenerNotOurs { .. })),
            "{:?}",
            refused.err()
        );
        assert!(seen.is_empty(), "nothing was sent: {seen:?}");
        let (accepted, seen) = squatted(uid).await;
        assert!(accepted.is_ok());
        assert!(
            matches!(seen.as_slice(), [Some(first), Some(second)] if first.starts_with("Bearer ") && first == second),
            "{seen:?}"
        );
    }
}
