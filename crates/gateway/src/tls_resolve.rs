use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use crate::server::BindSource;

const LOOPBACK_HOST: &str = "127.0.0.1";
const DEFAULT_GATEWAY_PORT: u16 = 5174;

impl BindSource {
    #[must_use]
    pub fn resolve(
        config: &config::Config,
        env: &config::Environment,
        node_name: &str,
        shared_dir: &Path,
    ) -> Self {
        Self::resolve_section(config.gateway.as_ref(), env, node_name, shared_dir)
    }

    #[must_use]
    pub fn resolve_section(
        section: Option<&config::model::GatewaySection>,
        env: &config::Environment,
        node_name: &str,
        shared_dir: &Path,
    ) -> Self {
        let mut notices = Vec::new();
        let mut cert = configured_text(section.and_then(|section| section.tls_cert.as_deref()))
            .or_else(|| {
                env_path(
                    env,
                    "RIVETHUB_TLS_CERT",
                    "RIVETOS_DEN_TLS_CERT",
                    &mut notices,
                )
            });
        let mut key = configured_text(section.and_then(|section| section.tls_key.as_deref()))
            .or_else(|| env_path(env, "RIVETHUB_TLS_KEY", "RIVETOS_DEN_TLS_KEY", &mut notices));
        if (cert.is_none() || key.is_none())
            && let Some((issued_cert, issued_key)) = issued_pair(shared_dir, node_name)
        {
            if cert.is_none() {
                cert = Some(issued_cert);
            }
            if key.is_none() {
                key = Some(issued_key);
            }
        }
        for notice in &notices {
            tracing::warn!("{notice}");
        }
        Self {
            host: host_of(section),
            port: port_of(section),
            tls_cert: cert,
            tls_key: key,
            notices,
        }
    }
}

fn host_of(section: Option<&config::model::GatewaySection>) -> String {
    match section.and_then(|section| section.host.clone()) {
        Some(host) => host,
        None => LOOPBACK_HOST.to_owned(),
    }
}

fn port_of(section: Option<&config::model::GatewaySection>) -> u16 {
    match section {
        Some(section) => section.port,
        None => DEFAULT_GATEWAY_PORT,
    }
}

#[must_use]
pub fn private_ca(
    section: Option<&config::model::GatewaySection>,
    env: &config::Environment,
    shared_dir: &Path,
) -> Option<PathBuf> {
    let path = section
        .and_then(|section| section.trust_root.clone())
        .or_else(|| {
            env_path(
                env,
                "RIVETHUB_TLS_CA",
                "RIVETOS_DEN_TLS_CA",
                &mut Vec::new(),
            )
        })
        .or_else(|| trimmed(env.text("RIVET_CA_CHAIN")).map(PathBuf::from))
        .unwrap_or_else(|| {
            shared_dir
                .join("rivet-ca")
                .join("intermediate")
                .join("chain.pem")
        });
    path.is_file().then_some(path)
}

fn configured_text(value: Option<&str>) -> Option<PathBuf> {
    trimmed(value).map(PathBuf::from)
}

fn env_path(
    env: &config::Environment,
    current: &str,
    legacy: &str,
    notices: &mut Vec<String>,
) -> Option<PathBuf> {
    env_value(env, current, legacy, notices).map(PathBuf::from)
}

fn env_value(
    env: &config::Environment,
    current: &str,
    legacy: &str,
    notices: &mut Vec<String>,
) -> Option<String> {
    if let Some(value) = trimmed(env.text(current)) {
        return Some(value.to_owned());
    }
    if let Some(notice) = env.legacy_notice(current, legacy) {
        notices.push(notice);
    }
    trimmed(env.text(legacy)).map(str::to_owned)
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

fn issued_pair(shared_dir: &Path, node_name: &str) -> Option<(PathBuf, PathBuf)> {
    let name = single_component(node_name)?;
    let root = shared_dir.join("rivet-ca").join("issued");
    let cert = contained_file(&root, &root.join(format!("{name}.crt")))?;
    let key = contained_file(&root, &root.join(format!("{name}.key")))?;
    Some((cert, key))
}

fn single_component(name: &str) -> Option<&str> {
    if name.is_empty() || name.contains(['/', '\\', '\0']) {
        return None;
    }
    let mut components = Path::new(name).components();
    match components.next() {
        Some(Component::Normal(part))
            if components.next().is_none() && part == OsStr::new(name) =>
        {
            Some(name)
        }
        Some(_) | None => None,
    }
}

fn contained_file(root: &Path, candidate: &Path) -> Option<PathBuf> {
    let root = canonical_directory(root)?;
    let file = canonical_file(candidate)?;
    let normalized_root = config::normalize(&root);
    let normalized_file = config::normalize(&file);
    if normalized_file.starts_with(&normalized_root) {
        Some(file)
    } else {
        None
    }
}

fn canonical_directory(path: &Path) -> Option<PathBuf> {
    let found = canonicalize(path)?;
    if found.is_dir() { Some(found) } else { None }
}

fn canonical_file(path: &Path) -> Option<PathBuf> {
    let found = canonicalize(path)?;
    if found.is_file() { Some(found) } else { None }
}

fn canonicalize(path: &Path) -> Option<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(found) => Some(found),
        Err(error) => unavailable(&error),
    }
}

fn unavailable(error: &std::io::Error) -> Option<PathBuf> {
    tracing::debug!(%error, "shared issued material is not available");
    None
}
