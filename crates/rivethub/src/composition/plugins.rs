use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use config::Environment;
use gateway::plugins::{HostSettings, PluginHost};

use super::ComposeError;

pub(crate) fn root(state_dir: &Path) -> PathBuf {
    state_dir.join("plugins")
}

pub(super) async fn started(
    gateway: gateway::Gateway,
    env: &Environment,
    node_ports: Vec<u16>,
) -> Result<Arc<gateway::Gateway>, ComposeError> {
    let host = host(gateway.settings(), env, node_ports).await?;
    let gateway = Arc::new(gateway.with_plugins(host));
    gateway::plugins::start(&gateway).await?;
    Ok(gateway)
}

async fn host(
    settings: &gateway::Settings,
    env: &Environment,
    node_ports: Vec<u16>,
) -> Result<Arc<PluginHost>, ComposeError> {
    let inherited = ["PATH", "HOME", "USER", "LANG", "LC_ALL", "TZ"]
        .into_iter()
        .filter_map(|name| {
            env.get(name)
                .map(|value| (OsString::from(name), value.to_owned()))
        })
        .collect();
    Ok(PluginHost::open(HostSettings {
        root: root(&settings.state_dir),
        workshop: settings.state_dir.join("workshop"),
        templates: settings.state_dir.join("plugin-templates"),
        node: settings.node.clone(),
        inherited,
        node_ports,
    })
    .await?)
}

pub(crate) fn tools_folder(
    config: &config::Config,
    env: &Environment,
) -> Result<PathBuf, ComposeError> {
    let node = crate::identity::resolve(config, env)?;
    let settings = gateway::Settings::from_config_in(config, env, node.hostname, node.name)?;
    Ok(root(&settings.state_dir))
}
