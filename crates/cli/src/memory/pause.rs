use std::str::FromStr;

use config::Environment;
use kernel::queue::pause::PausableRole;
use kernel::queue::pause::WorkerPauses as _;
use kernel::tenancy::Scope;
use kernel::tidy::UpkeepStore as _;
use memory_postgres::PostgresMemory;
use wire::gateway::worker_pauses::WorkerPauses;

use crate::argv::Scan;
use crate::{Error, Report};

const PAUSED_BY: &str = "cli";

#[derive(Clone, Copy)]
pub(super) enum Action {
    Status,
    Pause(PausableRole),
    Resume(PausableRole),
}

pub(super) struct Args {
    action: Action,
    json: bool,
}

pub(super) fn parse_pause(args: &[String]) -> Result<Args, Error> {
    let (role, json) = role_and_json(args)?;
    Ok(Args {
        action: role.map_or(Action::Status, Action::Pause),
        json,
    })
}

pub(super) fn parse_resume(args: &[String]) -> Result<Args, Error> {
    let (role, json) = role_and_json(args)?;
    Ok(Args {
        action: Action::Resume(role.ok_or(Error::PauseUsage)?),
        json,
    })
}

fn role_and_json(args: &[String]) -> Result<(Option<PausableRole>, bool), Error> {
    let mut scan = Scan::new(args);
    let mut role = None;
    let mut json = false;
    while let Some(arg) = scan.current().map(str::to_owned) {
        if scan.flag("--json") {
            json = true;
        } else if role.is_none() && !arg.starts_with('-') {
            role = Some(PausableRole::from_str(&arg)?);
            scan.bump();
        } else {
            return Err(super::parse::reject(&scan));
        }
    }
    Ok((role, json))
}

pub(super) async fn run(args: Args, env: &Environment) -> Result<Report, Error> {
    let memory = PostgresMemory::connect(&super::run::postgres_url(env)?).await?;
    let listed = Scope::Maintenance
        .run(async {
            match args.action {
                Action::Status => {}
                Action::Pause(role) => {
                    memory.pause(role, PAUSED_BY).await?;
                }
                Action::Resume(role) => memory.resume(role).await?,
            }
            let paused = memory.paused().await?;
            let tasks = memory.tasks().await?;
            Ok::<_, Error>(gateway::worker_pauses::listed(&paused, &tasks))
        })
        .await?;
    if args.json {
        return Ok(Report::success(format!(
            "{}\n",
            serde_json::to_string(&listed)?
        )));
    }
    Ok(Report::success(human(&listed)))
}

fn human(listed: &WorkerPauses) -> String {
    let lines: Vec<String> = listed
        .roles
        .iter()
        .map(|role| {
            let state = match (role.paused_at, &role.paused_by) {
                (Some(at), Some(by)) => format!("paused since {at} by {by}"),
                (Some(at), None) => format!("paused since {at}"),
                (None, _) => "running".to_owned(),
            };
            format!("{} {state}, {} queued", role.role, role.queued)
        })
        .collect();
    format!("{}\n", lines.join("\n"))
}
