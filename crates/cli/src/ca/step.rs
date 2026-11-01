use std::path::{Path, PathBuf};

use config::Environment;
use kernel::ca::Principal;
use tokio_util::sync::CancellationToken;

use super::TokenKind;
use super::paths::Material;
use crate::{Error, Exit, Report};

pub(super) struct Call<'a> {
    pub(super) material: &'a Material,
    pub(super) env: &'a Environment,
    pub(super) shutdown: &'a CancellationToken,
}

pub(super) async fn issue(
    kind: TokenKind,
    password: Option<PathBuf>,
    roster_path: Option<PathBuf>,
    call: &Call<'_>,
) -> Result<Report, Error> {
    let password = require_password(password).await?;
    let plan = match kind {
        TokenKind::Node { name } => {
            let roster = load_roster(roster_path).await?;
            pki::plan_cli(pki::CliToken::Node(name), Some(&roster))?
        }
        TokenKind::Agent { name } => {
            let roster = load_roster(roster_path).await?;
            pki::plan_cli(pki::CliToken::Agent(name), Some(&roster))?
        }
        TokenKind::Device { device, user } => {
            pki::plan_principal(&Principal::Device { user, device }, None)?
        }
    };
    let minted = pki::mint(
        &plan,
        &endpoint(call, &password),
        &process_env(call.env),
        call.shutdown,
    )
    .await?;
    Ok(Report {
        stdout: format!("{}\n", minted.token),
        stderr: minted.stderr,
        exit: Exit::Ok,
        stdout_raw: Vec::new(),
    })
}

pub(super) async fn revoke(
    serial: &str,
    password: Option<PathBuf>,
    call: &Call<'_>,
) -> Result<Report, Error> {
    let password = require_password(password).await?;
    pki::revoke_serial(
        serial,
        &endpoint(call, &password),
        &process_env(call.env),
        call.shutdown,
    )
    .await?;
    Ok(Report::success("Certificate revoked.\n"))
}

pub(super) fn connect(
    material: &Material,
    shutdown: &CancellationToken,
) -> Result<stepca::Authority, Error> {
    let config = stepca::Config::new(material.url.clone(), material.pem.clone())?;
    Ok(stepca::Authority::connect(config)?.with_cancellation(shutdown.clone()))
}

fn endpoint(call: &Call<'_>, password: &Path) -> pki::Endpoint {
    pki::Endpoint {
        url: call.material.url.clone(),
        root: call.material.root.clone(),
        password_file: password.to_path_buf(),
    }
}

fn process_env(env: &Environment) -> pki::ProcessEnv {
    pki::ProcessEnv::from_pairs(env.iter().map(|(key, value)| (key.clone(), value.clone())))
}

async fn load_roster(path: Option<PathBuf>) -> Result<pki::Roster, Error> {
    let path = path.ok_or(Error::RosterRequired)?;
    let text = tokio::task::spawn_blocking(move || super::paths::roster_text(&path)).await??;
    Ok(pki::parse_roster(&text)?)
}

async fn require_password(path: Option<PathBuf>) -> Result<PathBuf, Error> {
    let path = path.ok_or(Error::PasswordRequired)?;
    tokio::task::spawn_blocking(move || super::paths::password_file(&path)).await?
}
