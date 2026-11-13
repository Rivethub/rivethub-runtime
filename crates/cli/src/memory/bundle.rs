use std::path::Path;
use std::path::PathBuf;

use capture::receive::Blocked;
use capture::receive::Decisions;
use chrono::Utc;
use config::Config;
use config::Environment;
use memory_postgres::PostgresMemory;
use uuid::Uuid;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;
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

pub(super) fn ingest_args(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut path = None;
    let mut force = false;
    let mut dry_run = false;
    let mut decisions = Decisions::default();
    while let Some(token) = scan.current().map(str::to_owned) {
        if scan.flag("--force") {
            force = true;
            continue;
        }
        if super::hold::take_decision(&mut scan, &mut decisions)? {
            continue;
        }
        if scan.flag("--dry-run") {
            dry_run = true;
            continue;
        }
        if token.starts_with('-') {
            return Err(Error::UnknownOption(token));
        }
        if path.is_some() {
            return Err(Error::ImportOne);
        }
        path = Some(PathBuf::from(token));
        scan.bump();
    }
    let Some(path) = path else {
        return Err(Error::ImportPath);
    };
    Ok(MemoryCommand::IngestBundle {
        path,
        force,
        dry_run,
        decisions,
    })
}

pub(super) async fn ingest(
    path: &Path,
    force: bool,
    dry_run: bool,
    decisions: &Decisions,
    env: &Environment,
) -> Result<Report, Error> {
    let bundle = capture::bundle::parse(&tokio::fs::read_to_string(path).await?)?;
    let raw = bundle.manifest.sanitization == Sanitization::Raw;
    if raw && !force && !dry_run {
        return Err(Error::BundleRaw {
            path: path.display().to_string(),
        });
    }
    let capture::sanitize::OpenFlags {
        withheld,
        unreviewed,
    } = capture::sanitize::open_flags(&bundle)?;
    if withheld > 0 && !force && !dry_run {
        return Err(Error::BundleFlags {
            path: path.display().to_string(),
            count: withheld,
        });
    }
    let open = match (withheld, force) {
        (0, _) if unreviewed > 0 => format!(" ({unreviewed} advisory flags unreviewed)"),
        (0, _) => String::new(),
        (count, true) => format!(" ({count} withheld flags forced)"),
        (count, false) => format!(" ({count} withheld flags; ingest needs --force)"),
    };
    if raw {
        return ingest_raw(&bundle, force, dry_run, &open, env).await;
    }
    let checked = capture::receive::check(&bundle);
    let plan = match checked.plan(&bundle, decisions, force) {
        Ok(plan) => plan,
        Err(Blocked::Undecided { .. }) => {
            return Ok(super::hold::held_report(
                &checked,
                &bundle,
                "Rerun this command with your decisions.",
            ));
        }
        Err(blocked) => {
            return Err(Error::BundleBlocked {
                path: path.display().to_string(),
                source: blocked,
            });
        }
    };
    let decided = super::hold::decided(&plan);
    let shown: Vec<&BundledSession> = plan
        .imports
        .iter()
        .map(|accepted| accepted.session)
        .collect();
    let sessions = sessions_from(&shown);
    if dry_run {
        let mut lines: Vec<String> = shown.iter().map(|session| listed(session)).collect();
        lines.push(format!(
            "would ingest {sessions}{decided}{open}; nothing written\n"
        ));
        return Ok(Report::success(lines.concat()));
    }
    let imports: Vec<_> = plan
        .imports
        .iter()
        .map(|accepted| (accepted.session, Some(accepted.checks.clone())))
        .collect();
    let totals = if imports.is_empty() {
        capture::writer::Totals {
            inserted: 0,
            skipped: 0,
            conversation_id: String::new(),
        }
    } else {
        crate::capture_hook::ingest_bundle(env, &imports, None, Utc::now()).await?
    };
    Ok(Report::success(format!(
        "ingested {sessions}{decided}{open}: {} inserted, {} skipped\n",
        totals.inserted, totals.skipped
    )))
}

async fn ingest_raw(
    bundle: &SessionBundle,
    force: bool,
    dry_run: bool,
    open: &str,
    env: &Environment,
) -> Result<Report, Error> {
    let state = if force {
        " (raw bundle forced)"
    } else {
        " (raw bundle; ingest needs --force)"
    };
    let shown: Vec<&BundledSession> = bundle.sessions.iter().collect();
    let sessions = sessions_from(&shown);
    if dry_run {
        let mut lines: Vec<String> = shown.iter().map(|session| listed(session)).collect();
        lines.push(format!(
            "would ingest {sessions}{state}{open}; nothing written\n"
        ));
        return Ok(Report::success(lines.concat()));
    }
    let imports: Vec<_> = shown.iter().map(|session| (*session, None)).collect();
    let totals = crate::capture_hook::ingest_bundle(env, &imports, None, Utc::now()).await?;
    Ok(Report::success(format!(
        "ingested {sessions}{state}{open}: {} inserted, {} skipped\n",
        totals.inserted, totals.skipped
    )))
}

fn sessions_from(sessions: &[&BundledSession]) -> String {
    let mut authors: Vec<String> = Vec::new();
    for session in sessions {
        let author = capture::receive::visible(author_of(session));
        if !authors.contains(&author) {
            authors.push(author);
        }
    }
    let count = sessions.len();
    let noun = if count == 1 { "session" } else { "sessions" };
    if authors.is_empty() {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun} from {}", authors.join(", "))
    }
}

fn listed(session: &BundledSession) -> String {
    let origin = &session.origin;
    let count = session.batch.messages.len();
    let noun = if count == 1 { "message" } else { "messages" };
    let dates = match (origin.created_at, origin.updated_at) {
        (Some(created), Some(updated)) => format!(
            "{} to {}",
            created.format("%Y-%m-%d"),
            updated.format("%Y-%m-%d")
        ),
        _ => "undated".to_owned(),
    };
    format!(
        "{} ({}): {count} {noun} from {}, {dates}\n",
        capture::receive::visible(&origin.session_key),
        capture::receive::visible(&origin.agent),
        capture::receive::visible(author_of(session)),
    )
}

fn author_of(session: &BundledSession) -> &str {
    session.origin.known_author().unwrap_or("an unknown author")
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
    let sessions: Vec<BundledSession> = sessions
        .into_iter()
        .map(BundledSession::forwarding_stored_origin)
        .collect();
    let count = sessions.len();
    let mut text = capture::bundle::seal(author, Utc::now(), Sanitization::Raw, sessions)?.text;
    text.push(b'\n');
    capture::share::write_file(out.clone(), text, capture::share::Clobber::Replace).await?;
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

pub(super) async fn exporter(env: &Environment) -> Result<BundleAuthor, Error> {
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
