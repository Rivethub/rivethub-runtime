use std::time::Duration;

use clap::Subcommand;
use config::Environment;

use crate::status::{self, Endpoint};
use crate::{Error, Report, configuration, process};

const ROTATE_TIMEOUT: Duration = Duration::from_secs(10);
const ROTATE_PATH: &str = "api/auth/rotate";

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    #[command(about = "Print where the local owner token file lives")]
    TokenPath {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Replace the local owner token and end every browser session")]
    Rotate,
}

pub(crate) async fn run(command: AuthCommand, env: &Environment) -> Result<Report, Error> {
    match command {
        AuthCommand::TokenPath { json } => token_path(env, json),
        AuthCommand::Rotate => rotate(env).await,
    }
}

#[derive(serde::Serialize)]
struct TokenPath<'a> {
    path: &'a str,
}

fn token_path(env: &Environment, json: bool) -> Result<Report, Error> {
    let path = gateway::local_owner::token_path(&gateway::state_dir(env)?);
    let text = path.display().to_string();
    Ok(Report::success(if json {
        format!("{}\n", serde_json::to_string(&TokenPath { path: &text })?)
    } else {
        format!("{text}\n")
    }))
}

async fn rotate(env: &Environment) -> Result<Report, Error> {
    let (_, loaded) = configuration::load(env).await?;
    let reached = match status::gateway(env, &loaded.config, ROTATE_TIMEOUT).await {
        Ok(Endpoint { url, client }) => client.post(url.join(ROTATE_PATH)?).send().await,
        Err(Error::ListenerNotOurs { .. } | Error::GatewayNotRunning { .. }) => {
            return remove_unowned(env).await;
        }
        Err(error) => return Err(error),
    };
    match reached {
        Ok(response) if response.status().is_success() => {
            Ok(Report::success("Rotated the local owner token.\n"))
        }
        Ok(response) => Err(Error::AuthRotate {
            status: response.status().as_u16(),
        }),
        Err(error) if error.is_connect() => remove_unowned(env).await,
        Err(error) => Err(error.into()),
    }
}

async fn remove_unowned(env: &Environment) -> Result<Report, Error> {
    if let Some(pid) = process::read_pid(env).await?
        && nix::sys::signal::kill(pid, None).is_ok()
    {
        return Err(Error::AuthRotateUnreachable { pid: pid.as_raw() });
    }
    let path = gateway::local_owner::token_path(&gateway::state_dir(env)?);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(Report::success(
        "No gateway of this state directory is running; removed the local owner token file. The next start makes a new one.\n",
    ))
}
