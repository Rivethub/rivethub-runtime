use std::path::Path;

use config::Environment;
use gateway::account::{AccountError, AccountFiles, Cloud, CloudSettings, fetch, latest};
use tokio_util::sync::CancellationToken;
use wire::gateway::account::ReleaseChannel;

use super::{Channel, UpdateArgs, UpdateError, release};

pub(super) async fn stage(
    config: &config::Config,
    env: &Environment,
    args: &UpdateArgs,
    directory: &Path,
    shutdown: &CancellationToken,
) -> Result<String, UpdateError> {
    if args.version.is_some() {
        return Err(UpdateError::CloudVersion);
    }
    let channel = match args.channel {
        Channel::Stable => ReleaseChannel::Stable,
        Channel::Beta => ReleaseChannel::Beta,
        Channel::Nightly => return Err(UpdateError::Source),
    };
    let settings = CloudSettings::from_config(config)?;
    let files = AccountFiles::new(&gateway::state_dir(env)?);
    let reading = files.clone();
    let state = tokio::task::spawn_blocking(move || reading.read()).await??;
    let token = state
        .as_ref()
        .and_then(gateway::account::InstallState::token)
        .ok_or(AccountError::NotSignedIn)?;
    let cloud = Cloud::new(settings.base.clone())?;
    let (manifest, digest) = latest(&cloud, &settings, token, channel)
        .await?
        .ok_or(UpdateError::NoCloudRelease(channel))?;
    let verified = fetch(&cloud, &files, manifest, digest, shutdown).await?;
    let directory = directory.to_owned();
    let version = format!("v{}", verified.manifest.version);
    tokio::task::spawn_blocking(move || release::unpack(&verified.archive, &directory)).await??;
    Ok(version)
}
