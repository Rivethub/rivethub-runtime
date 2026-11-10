use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

use chrono::Utc;
use config::Config;
use config::Environment;
use memory_postgres::PostgresMemory;
use uuid::Uuid;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::gateway::users::DEFAULT_OWNER_ID;
use wire::gateway::users::UsersRegistry;

use super::parse::MemoryCommand;
use super::parse::nonempty;
use super::parse::reject;
use super::run::open_sqlite;
use super::target::Kind;
use super::target::Target;
use super::target::resolve;
use crate::Error;
use crate::Report;
use crate::argv::Scan;
use crate::argv::Taken;

pub(super) fn args(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut out = None;
    let mut sessions = Vec::new();
    let mut sqlite = None;
    while scan.current().is_some() {
        match scan.option("--session", None) {
            Taken::Value(value) if !value.trim().is_empty() => {
                sessions.push(value.trim().to_owned());
                continue;
            }
            Taken::Value(_) | Taken::Missing => return Err(Error::SessionRequired),
            Taken::Absent => {}
        }
        match scan.option("--out", None) {
            Taken::Value(value) => {
                out = Some(nonempty(&value, Error::OutRequired)?);
                continue;
            }
            Taken::Missing => return Err(Error::OutRequired),
            Taken::Absent => {}
        }
        match scan.option("--sqlite", None) {
            Taken::Value(value) => {
                sqlite = Some(nonempty(&value, Error::SqliteRequired)?);
                continue;
            }
            Taken::Missing => return Err(Error::SqliteRequired),
            Taken::Absent => {}
        }
        return Err(reject(&scan));
    }
    let Some(out) = out else {
        return Err(Error::OutRequired);
    };
    Ok(MemoryCommand::Bundle {
        out,
        sessions,
        sqlite,
    })
}

pub(super) async fn share(
    out: PathBuf,
    wanted: Vec<String>,
    sqlite: Option<PathBuf>,
    env: &Environment,
) -> Result<Report, Error> {
    if tokio::fs::metadata(&out)
        .await
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(Error::BundleOutDirectory(out.display().to_string()));
    }
    let target = resolve(env, sqlite, Kind::Either)?;
    let author = exporter(env).await?;
    let named = (!wanted.is_empty()).then_some(wanted);
    let sessions = match target {
        Target::Postgres(url) => {
            let ids = named.as_deref().map(conversation_ids).transpose()?;
            PostgresMemory::connect(&url)
                .await?
                .bundled_sessions(&author, ids.as_deref())
                .await?
        }
        Target::Sqlite(path) => {
            open_sqlite(&path)
                .await?
                .bundled_sessions(author.clone(), named.clone())
                .await?
        }
    };
    if let Some(named) = &named {
        refuse_missing(named, &sessions)?;
    }
    let count = sessions.len();
    let mut text = capture::bundle::seal(author, Utc::now(), sessions)?.text;
    text.push(b'\n');
    write_whole(out.clone(), text).await?;
    let noun = if count == 1 { "session" } else { "sessions" };
    Ok(Report::success(format!(
        "bundled {count} {noun} (raw, not sanitized) to {}\n",
        out.display()
    )))
}

fn conversation_ids(wanted: &[String]) -> Result<Vec<Uuid>, Error> {
    wanted
        .iter()
        .map(|id| Uuid::parse_str(id).map_err(|_| Error::BundleUnknown(id.clone())))
        .collect()
}

fn refuse_missing(wanted: &[String], sessions: &[BundledSession]) -> Result<(), Error> {
    let missing: Vec<&str> = wanted
        .iter()
        .filter(|id| {
            !sessions
                .iter()
                .any(|session| session.origin.conversation_id.eq_ignore_ascii_case(id))
        })
        .map(String::as_str)
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Error::BundleUnknown(missing.join(", ")))
    }
}

async fn write_whole(out: PathBuf, bytes: Vec<u8>) -> Result<(), Error> {
    tokio::task::spawn_blocking(move || {
        let parent = match out.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            Some(_) | None => Path::new("."),
        };
        let mut partial = tempfile::Builder::new()
            .prefix(".rh-bundle-")
            .suffix(".tmp")
            .tempfile_in(parent)?;
        partial.write_all(&bytes)?;
        partial.persist(&out).map_err(|error| error.error)?;
        Ok::<_, std::io::Error>(())
    })
    .await??;
    Ok(())
}

async fn exporter(env: &Environment) -> Result<BundleAuthor, Error> {
    let config = super::target::load(env)?.unwrap_or_default();
    let hostname = nix::unistd::gethostname()
        .map_err(|_| Error::HostName)?
        .into_string()
        .map_err(|_| Error::HostName)?;
    let node = config::resolve_node_name(&config, env, hostname.trim())?;
    Ok(BundleAuthor {
        user: node_owner(&config, env).await?,
        node,
    })
}

async fn node_owner(config: &Config, env: &Environment) -> Result<String, Error> {
    let path = env.users_file().map(PathBuf::from).or_else(|| {
        config
            .gateway
            .as_ref()
            .and_then(|gateway| gateway.users_file.clone())
    });
    let Some(path) = path else {
        return Ok(DEFAULT_OWNER_ID.to_owned());
    };
    let text = tokio::fs::read(&path).await?;
    let users: UsersRegistry =
        serde_json::from_slice(&text).map_err(|source| Error::BundleUsers {
            path: path.display().to_string(),
            source,
        })?;
    Ok(users.owner_user_id)
}
