use std::path::PathBuf;
use std::str::FromStr;

use config::Environment;
use kernel::queue::pause::PausableRole;
use kernel::queue::pause::WorkerPauses as PauseStore;
use kernel::tenancy::Scope;
use kernel::tidy::UpkeepStore;
use memory_postgres::PostgresMemory;
use wire::gateway::worker_pauses::WorkerPauses;

use super::target::Kind;
use super::target::Target;
use super::target::resolve;
use crate::argv::Scan;
use crate::argv::Taken;
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
    sqlite: Option<PathBuf>,
}

pub(super) fn parse_pause(args: &[String]) -> Result<Args, Error> {
    let (role, json, sqlite) = role_and_json(args)?;
    Ok(Args {
        action: role.map_or(Action::Status, Action::Pause),
        json,
        sqlite,
    })
}

pub(super) fn parse_resume(args: &[String]) -> Result<Args, Error> {
    let (role, json, sqlite) = role_and_json(args)?;
    Ok(Args {
        action: Action::Resume(role.ok_or(Error::PauseUsage)?),
        json,
        sqlite,
    })
}

fn role_and_json(args: &[String]) -> Result<(Option<PausableRole>, bool, Option<PathBuf>), Error> {
    let mut scan = Scan::new(args);
    let mut role = None;
    let mut json = false;
    let mut sqlite = None;
    while let Some(arg) = scan.current().map(str::to_owned) {
        match scan.option("--sqlite", None) {
            Taken::Value(value) => {
                sqlite = Some(super::parse::nonempty(&value, Error::SqliteRequired)?);
                continue;
            }
            Taken::Missing => return Err(Error::SqliteRequired),
            Taken::Absent => {}
        }
        if scan.flag("--json") {
            json = true;
        } else if role.is_none() && !arg.starts_with('-') {
            role = Some(PausableRole::from_str(&arg)?);
            scan.bump();
        } else {
            return Err(super::parse::reject(&scan));
        }
    }
    Ok((role, json, sqlite))
}

pub(super) async fn run(args: Args, env: &Environment) -> Result<Report, Error> {
    let listed = match resolve(env, args.sqlite.clone(), Kind::Either)? {
        Target::Postgres(url) => apply(&PostgresMemory::connect(&url).await?, args.action).await?,
        Target::Sqlite(path) => apply(&super::run::open_sqlite(&path).await?, args.action).await?,
    };
    if args.json {
        return Ok(Report::success(format!(
            "{}\n",
            serde_json::to_string(&listed)?
        )));
    }
    Ok(Report::success(human(&listed)))
}

async fn apply<S>(store: &S, action: Action) -> Result<WorkerPauses, Error>
where
    S: PauseStore + UpkeepStore,
{
    Scope::Maintenance
        .run(async {
            match action {
                Action::Status => {}
                Action::Pause(role) => {
                    store.pause(role, PAUSED_BY).await?;
                }
                Action::Resume(role) => store.resume(role).await?,
            }
            let paused = store.paused().await?;
            let tasks = store.tasks().await?;
            Ok(gateway::worker_pauses::listed(&paused, &tasks))
        })
        .await
}

fn human(listed: &WorkerPauses) -> String {
    let lines: Vec<String> = listed
        .roles
        .iter()
        .map(|role| {
            let state = match (role.paused_at, role.paused_by.as_deref()) {
                (Some(at), Some(by)) => format!("paused since {at} by {by}"),
                (Some(at), None) => format!("paused since {at}"),
                (None, Some(_) | None) => "running".to_owned(),
            };
            format!("{} {state}, {} queued", role.role, role.queued)
        })
        .collect();
    format!("{}\n", lines.join("\n"))
}
