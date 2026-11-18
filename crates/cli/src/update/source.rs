use std::path::{Component, Path};

use config::Environment;
use tokio_util::sync::CancellationToken;

use super::{Channel, UpdateArgs, UpdateError, files, release};
use crate::service::command::{capture, command, require_success};

pub(super) async fn stage(
    args: &UpdateArgs,
    env: &Environment,
    directory: &Path,
    shutdown: &CancellationToken,
) -> Result<String, UpdateError> {
    let root = env.install_root().ok_or(UpdateError::Source)?;
    let requested = Path::new(root);
    if !requested.is_absolute()
        || requested
            .components()
            .any(|part| part == Component::ParentDir)
    {
        return Err(UpdateError::Path(requested.to_owned()));
    }
    let root = tokio::fs::canonicalize(requested).await?;
    let branch = match args.channel {
        Channel::Nightly => "main",
        Channel::Stable | Channel::Beta => return Err(UpdateError::Source),
    };
    if git(&root, &["status", "--porcelain"], env, shutdown)
        .await?
        .is_empty()
        && git(&root, &["branch", "--show-current"], env, shutdown).await? == branch
    {
        git(
            &root,
            &["pull", "--ff-only", "origin", branch],
            env,
            shutdown,
        )
        .await?;
    } else {
        return Err(UpdateError::Branch(branch));
    }
    let revision = git(&root, &["rev-parse", "HEAD"], env, shutdown).await?;
    if let Some(version) = &args.version {
        release::validate_version(version)?;
        let requested = git(
            &root,
            &["rev-parse", "--verify", &format!("{version}^{{commit}}")],
            env,
            shutdown,
        )
        .await?;
        if revision != requested {
            return Err(UpdateError::Revision);
        }
    }
    let target = directory.join("build");
    let mut build = command("cargo", env);
    build
        .current_dir(&root)
        .args([
            "build",
            "--release",
            "--locked",
            "--bin",
            "rh",
            "--bin",
            "rivethub-migrate",
            "--target-dir",
        ])
        .arg(&target);
    require_success(capture(build, "cargo", shutdown).await?)?;
    let directory = directory.to_owned();
    tokio::task::spawn_blocking(move || {
        for name in ["rh", "rivethub-migrate"] {
            let binary = target.join("release").join(name);
            files::regular(&directory, &binary)?;
            std::fs::copy(binary, directory.join(name))?;
        }
        Ok::<_, UpdateError>(())
    })
    .await??;
    Ok(revision)
}

pub(super) async fn runtime_version(
    staging: &Path,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<String, UpdateError> {
    let mut rh = tokio::process::Command::new(staging.join("rh"));
    rh.env_clear()
        .envs(env.iter())
        .kill_on_drop(true)
        .arg("version");
    let result = require_success(capture(rh, "rh", shutdown).await?)?;
    let version = result
        .stdout
        .strip_prefix("RivetHub v")
        .and_then(|version| version.split_whitespace().next())
        .ok_or_else(|| UpdateError::Version(result.stdout.clone()))?;
    semver::Version::parse(version).map_err(|_| UpdateError::Version(version.to_owned()))?;
    Ok(version.to_owned())
}

async fn git(
    root: &Path,
    args: &[&str],
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<String, UpdateError> {
    let mut git = command("git", env);
    git.current_dir(root).args(args);
    Ok(require_success(capture(git, "git", shutdown).await?)?
        .stdout
        .trim()
        .to_owned())
}
