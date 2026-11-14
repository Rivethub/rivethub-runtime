use std::sync::Arc;

use config::{Config, Environment};
use gateway::Gateway;
use gateway::account::{AccountService, CloudSettings, CrashJob, Hubs};
use gateway::server::BindSource;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::ComposeError;

pub(super) async fn attach(
    gateway: Gateway,
    config: &Config,
    env: &Environment,
    bind: &BindSource,
    shutdown: &CancellationToken,
    tracker: &TaskTracker,
) -> Result<Gateway, ComposeError> {
    crate::telemetry::arm(config, env, CrashJob::Gateway);
    let settings = gateway.settings();
    let hubs = Hubs::new(
        &settings.node,
        serving_url(bind, &settings.hostname),
        config.hubs.clone().unwrap_or_default(),
        private_roots(settings.private_ca.as_deref()).await,
    )?;
    let service = Arc::new(
        AccountService::open(
            &settings.state_dir,
            CloudSettings::from_config(config)?,
            hubs,
        )
        .await?
        .with_login(Arc::new(cli::account_login::NodeAccountLogin::new(
            env.clone(),
            &CloudSettings::from_config(config)?.base,
        ))),
    );
    tracker.spawn(Arc::clone(&service).run(shutdown.child_token()));
    Ok(gateway.with_account(service))
}

fn serving_url(bind: &BindSource, hostname: &str) -> String {
    let scheme = if bind.tls_cert.is_some() {
        "https"
    } else {
        "http"
    };
    let unspecified = ["0.0.0.0", "::", "[::]"].contains(&bind.host.as_str());
    let host = if unspecified {
        hostname.to_owned()
    } else if bind.host.contains(':') && !bind.host.starts_with('[') {
        format!("[{}]", bind.host)
    } else {
        bind.host.clone()
    };
    format!("{scheme}://{host}:{}", bind.port)
}

async fn private_roots(path: Option<&std::path::Path>) -> Vec<reqwest::Certificate> {
    let Some(path) = path else {
        return Vec::new();
    };
    let pem = match tokio::fs::read(path).await {
        Ok(pem) => pem,
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "hub probes use the system roots only");
            return Vec::new();
        }
    };
    match reqwest::Certificate::from_pem_bundle(&pem) {
        Ok(roots) => roots,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "the mesh CA bundle does not parse; hub probes use the system roots only");
            Vec::new()
        }
    }
}
