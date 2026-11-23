use std::path::PathBuf;
use wire::time::TimestampFormat as _;

use config::Environment;
use kernel::memory::MemoryStore;
use kernel::owner::Owner;
use kernel::tenancy::{Scope, Viewer};
use memory_postgres::PostgresMemory;
use wire::gateway::users::DEFAULT_OWNER_ID;

use super::target::{Kind, Target, resolve};
use crate::argv::{Scan, Taken};
use crate::{Error, Report};

pub(super) struct Args {
    session: String,
    sqlite: Option<PathBuf>,
    json: bool,
}

pub(super) fn parse(args: &[String]) -> Result<Args, Error> {
    let mut scan = Scan::new(args);
    let mut session = None;
    let mut sqlite = None;
    let mut json = false;
    while let Some(arg) = scan.current().map(str::to_owned) {
        if scan.flag("--json") {
            json = true;
            continue;
        }
        match scan.option("--sqlite", None) {
            Taken::Value(path) => sqlite = Some(PathBuf::from(path)),
            Taken::Absent if arg.starts_with('-') => return Err(Error::UnknownOption(arg)),
            Taken::Absent if session.is_none() && !arg.trim().is_empty() => {
                session = Some(arg);
                scan.bump();
            }
            Taken::Missing | Taken::Absent => return Err(Error::SettleUsage),
        }
    }
    Ok(Args {
        session: session.ok_or(Error::SettleUsage)?,
        sqlite,
        json,
    })
}

pub(super) async fn run(args: Args, env: &Environment) -> Result<Report, Error> {
    let explicit = args.sqlite.is_some();
    let target = resolve(env, args.sqlite, Kind::Either)?;
    let owner = match super::target::load_for(env, explicit)? {
        Some(config) => super::bundle::node_owner(&config, env).await?,
        None if explicit => super::bundle::node_owner(&config::Config::default(), env).await?,
        None => DEFAULT_OWNER_ID.to_owned(),
    };
    let owner = Owner::new(owner)?;
    let store: Box<dyn MemoryStore> = match target {
        Target::Postgres(url) => Box::new(PostgresMemory::connect(&url).await?),
        Target::Sqlite(path) => Box::new(super::run::open_sqlite(&path).await?),
    };
    let scope = Scope::Viewer(Viewer::new(owner.clone(), Vec::new()));
    let result = scope
        .run(store.mark_summary_settled(&args.session, &owner))
        .await?
        .ok_or(Error::SettleMissing(args.session))?;
    if args.json {
        return Ok(Report::success(format!(
            "{}\n",
            serde_json::to_string(&result)?
        )));
    }
    let status = result.status.to_string().replace('_', " ");
    let at = result
        .settled_at
        .map(|at| format!(" at {}", at.rfc3339_offset()))
        .unwrap_or_default();
    Ok(Report::success(format!(
        "{} revision {} {status}{at}\n",
        result.conversation_id, result.input_revision
    )))
}
