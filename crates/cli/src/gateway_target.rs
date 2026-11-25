use std::net::SocketAddr;

use config::{Config, Environment};
use gateway::server::BindSource;

use crate::Error;
use crate::status::{host_name, listen_url, served_pin};

pub(crate) struct GatewayTarget {
    pub(crate) source: BindSource,
    pub(crate) url: url::Url,
    pub(crate) ca: Option<std::path::PathBuf>,
    pub(crate) pin: Option<(String, SocketAddr)>,
    pub(crate) explicit: bool,
}

impl GatewayTarget {
    pub(crate) fn sends_owner_token(&self) -> bool {
        !self.explicit
            || match self.url.host() {
                Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
                Some(url::Host::Ipv4(address)) => address.is_loopback(),
                Some(url::Host::Ipv6(address)) => address.is_loopback(),
                None => false,
            }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Locate {
    Discover,
    ThisNode,
    Configured,
}

pub(crate) const GATEWAY_URL_ENV: &str = "RIVETHUB_GATEWAY_URL";

pub(crate) async fn gateway_target(
    env: &Environment,
    config: &Config,
) -> Result<GatewayTarget, Error> {
    target_for(env, config, Locate::Discover).await
}

pub(crate) async fn target_for(
    env: &Environment,
    config: &Config,
    locate: Locate,
) -> Result<GatewayTarget, Error> {
    let explicit = match locate {
        Locate::Discover => env
            .text(GATEWAY_URL_ENV)
            .map(str::trim)
            .filter(|text| !text.is_empty()),
        Locate::ThisNode | Locate::Configured => None,
    };
    if let Some(raw) = explicit {
        return explicit_target(env, config, raw);
    }
    let hostname = host_name()?;
    let node = config::resolve_node_name(config, env, &hostname)?;
    let section = config.gateway.clone();
    let tls_env = env.clone();
    let shared = config::shared_dir(&tls_env);
    let mut source = tokio::task::spawn_blocking(move || {
        BindSource::resolve_section(section.as_ref(), &tls_env, &node, &shared)
    })
    .await?;
    if matches!(locate, Locate::Discover | Locate::ThisNode) {
        source = running_bind(env, source)?;
    }
    let mut url = listen_url(&source)?;
    let ca = gateway::private_ca(config.gateway.as_ref(), env, &config::shared_dir(env));
    let pin = match (&source.tls_cert, &ca) {
        (Some(cert), Some(_)) => served_pin(&mut url, cert, source.port)?,
        _ => None,
    };
    Ok(GatewayTarget {
        source,
        url,
        ca,
        pin,
        explicit: false,
    })
}

fn running_bind(env: &Environment, mut source: BindSource) -> Result<BindSource, Error> {
    let Some(record) = gateway::runtime_record::read_live(&gateway::state_dir(env)?) else {
        return Ok(source);
    };
    if record.tls != source.tls_cert.is_some() {
        tracing::debug!("the running gateway's TLS differs from the config; using the config");
        return Ok(source);
    }
    source.port = record.port;
    if !record.tls && !record.host.is_unspecified() {
        source.host = record.host.to_string();
    }
    Ok(source)
}

fn explicit_target(env: &Environment, config: &Config, raw: &str) -> Result<GatewayTarget, Error> {
    let mut url = url::Url::parse(raw)?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(Error::GatewayUrlSuffix);
    }
    let tls = match url.scheme() {
        "https" => true,
        "http" => false,
        scheme => return Err(Error::GatewayScheme(scheme.to_owned())),
    };
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    let host = url
        .host_str()
        .ok_or(Error::Address)?
        .trim_matches(['[', ']'])
        .to_owned();
    let port = url.port_or_known_default().ok_or(Error::Address)?;
    let ca = if tls {
        gateway::private_ca(config.gateway.as_ref(), env, &config::shared_dir(env))
    } else {
        None
    };
    Ok(GatewayTarget {
        source: BindSource {
            host,
            port,
            tls_cert: None,
            tls_key: None,
            notices: Vec::new(),
        },
        url,
        ca,
        pin: None,
        explicit: true,
    })
}
