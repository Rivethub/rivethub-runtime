mod paths;
mod publish;
mod status;
mod step;

use std::path::PathBuf;

use clap::{Args, Subcommand};
use config::Environment;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{Error, Report};

#[derive(Debug, Args)]
pub struct CaArgs {
    #[arg(long, global = true)]
    pub ca_url: Option<Url>,
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,
    #[arg(long, global = true)]
    pub provisioner_password_file: Option<PathBuf>,
    #[arg(long, global = true)]
    pub roster: Option<PathBuf>,
    #[arg(long, global = true)]
    pub crl_out: Option<PathBuf>,
    #[command(subcommand)]
    pub command: CaCommand,
}

#[derive(Debug, Subcommand)]
pub enum CaCommand {
    #[command(about = "Mint a one-time step-ca token")]
    Token {
        #[command(subcommand)]
        kind: TokenKind,
    },
    #[command(about = "Revoke one certificate by decimal serial")]
    Revoke { serial: String },
    #[command(about = "Print the certificate revocation list")]
    Crl,
    #[command(about = "Show the certificate authority roots")]
    Status,
}

#[derive(Debug, Subcommand)]
pub enum TokenKind {
    #[command(about = "Mint a token for a node in the operator roster")]
    Node { name: String },
    #[command(about = "Mint a token for an agent in the operator roster")]
    Agent { name: String },
    #[command(about = "Mint a token for a user's device")]
    Device {
        device: String,
        #[arg(long)]
        user: String,
    },
}

pub(crate) async fn authority_root(root: PathBuf, url: Url) -> Result<PathBuf, Error> {
    let material = paths::load(root, url).await?;
    Ok(material.root)
}

pub(crate) async fn password_file(path: PathBuf) -> Result<PathBuf, Error> {
    tokio::task::spawn_blocking(move || paths::password_file(&path)).await?
}

pub(crate) async fn roster(path: PathBuf) -> Result<pki::Roster, Error> {
    let text = tokio::task::spawn_blocking(move || paths::roster_text(&path)).await??;
    Ok(pki::parse_roster(&text)?)
}

pub(crate) async fn run(
    args: CaArgs,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Report, Error> {
    let CaArgs {
        ca_url,
        root,
        provisioner_password_file,
        roster,
        crl_out,
        command,
    } = args;
    let ca_url = ca_url.ok_or(Error::CaUrlRequired)?;
    let root = root.ok_or(Error::RootRequired)?;
    let material = paths::load(root, ca_url).await?;
    let call = step::Call {
        material: &material,
        env,
        shutdown,
    };
    match command {
        CaCommand::Token { kind } => {
            step::issue(kind, provisioner_password_file, roster, &call).await
        }
        CaCommand::Revoke { serial } => {
            step::revoke(&serial, provisioner_password_file, &call).await
        }
        CaCommand::Crl => publish::crl(crl_out, &material, shutdown).await,
        CaCommand::Status => status::report(&material, shutdown).await,
    }
}
