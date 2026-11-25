use jiff::Timestamp;
use std::path::Path;
use std::path::PathBuf;
use wire::time::LegacyStrftime as _;

use capture::receive::Blocked;
use capture::receive::Decisions;
use config::Config;
use config::Environment;
use memory_postgres::PostgresMemory;
use uuid::Uuid;
use wire::capture::BundleAuthor;
use wire::capture::BundleFilter;
use wire::capture::BundledSession;
use wire::capture::OwnerScope;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;
use wire::gateway::users::DEFAULT_OWNER_ID;
use wire::gateway::users::UsersRegistry;

use super::parse::MemoryCommand;
use super::parse::nonempty;
use super::parse::reject;
use super::run::open_sqlite_read_only;
use super::target::Kind;
use super::target::Target;
use super::target::resolve;
use crate::Error;
use crate::Report;
use crate::argv::Scan;
use crate::argv::Taken;

pub(super) struct BundleJob {
    out: Option<PathBuf>,
    sessions: Vec<String>,
    sqlite: Option<PathBuf>,
    filter: BundleFilter,
    dry_run: bool,
}

pub(super) fn args(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut out = None;
    let mut sessions = Vec::new();
    let mut sqlite = None;
    let mut owners = Vec::new();
    let mut all_owners = false;
    let mut filter = BundleFilter::default();
    let mut dry_run = false;
    while scan.current().is_some() {
        match scan.option("--session", None) {
            Taken::Value(value) if !value.trim().is_empty() => {
                sessions.push(value.trim().to_owned());
                continue;
            }
            Taken::Value(_) | Taken::Missing => return Err(Error::SessionRequired),
            Taken::Absent => {}
        }
        match scan.option("--owner", None) {
            Taken::Value(value) if !value.trim().is_empty() => {
                owners.push(value.trim().to_owned());
                continue;
            }
            Taken::Value(_) | Taken::Missing => return Err(Error::OwnerRequired),
            Taken::Absent => {}
        }
        if scan.flag("--all-owners") {
            all_owners = true;
            continue;
        }
        if scan.flag("--dry-run") {
            dry_run = true;
            continue;
        }
        if take_window(&mut scan, &mut filter)? {
            continue;
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
    filter.owners = match (all_owners, owners.is_empty()) {
        (true, false) => return Err(Error::BundleOwnerConflict),
        (true, true) => OwnerScope::All,
        (false, false) => OwnerScope::Listed(owners),
        (false, true) => OwnerScope::Node,
    };
    let windowed = filter.since.is_some() || filter.until.is_some() || filter.limit.is_some();
    if windowed && !sessions.is_empty() {
        return Err(Error::BundleSessionWindow);
    }
    if out.is_none() && !dry_run {
        return Err(Error::OutRequired);
    }
    Ok(MemoryCommand::Bundle(BundleJob {
        out,
        sessions,
        sqlite,
        filter,
        dry_run,
    }))
}

fn take_window(scan: &mut Scan<'_>, filter: &mut BundleFilter) -> Result<bool, Error> {
    match scan.option("--since", None) {
        Taken::Value(value) => {
            filter.since = Some(when("--since", &value)?);
            return Ok(true);
        }
        Taken::Missing => return Err(Error::SinceRequired),
        Taken::Absent => {}
    }
    match scan.option("--until", None) {
        Taken::Value(value) => {
            filter.until = Some(when("--until", &value)?);
            return Ok(true);
        }
        Taken::Missing => return Err(Error::UntilRequired),
        Taken::Absent => {}
    }
    match scan.option("--limit", None) {
        Taken::Value(value) => {
            filter.limit = Some(match value.parse::<u32>() {
                Ok(count) if count > 0 => count,
                _ => return Err(Error::LimitPositive),
            });
            Ok(true)
        }
        Taken::Missing => Err(Error::LimitInteger),
        Taken::Absent => Ok(false),
    }
}

fn when(flag: &'static str, text: &str) -> Result<Timestamp, Error> {
    super::parse::timestamp(text).map_err(|_| Error::BundleTime {
        flag,
        text: text.to_owned(),
    })
}

pub(super) fn ingest_args(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut path = None;
    let mut force = false;
    let mut dry_run = false;
    let mut decisions = Decisions::default();
    let mut gateway = None;
    while let Some(token) = scan.current().map(str::to_owned) {
        if scan.flag("--force") {
            force = true;
            continue;
        }
        if crate::argv::take_gateway(&mut scan, &mut gateway)? {
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
        gateway,
    })
}

pub(super) async fn ingest(
    path: &Path,
    force: bool,
    dry_run: bool,
    decisions: &Decisions,
    gateway: Option<&str>,
    env: &Environment,
) -> Result<Report, Error> {
    let env = &crate::argv::with_gateway(env, gateway);
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
        crate::capture_hook::ingest_bundle(env, &imports, None, Timestamp::now()).await?
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
    let totals = crate::capture_hook::ingest_bundle(env, &imports, None, Timestamp::now()).await?;
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
            created.legacy_strftime("%Y-%m-%d"),
            updated.legacy_strftime("%Y-%m-%d")
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

pub(super) async fn share(job: BundleJob, env: &Environment) -> Result<Report, Error> {
    let BundleJob {
        out,
        sessions: wanted,
        sqlite,
        filter,
        dry_run,
    } = job;
    if let Some(out) = out.as_ref().filter(|_| !dry_run)
        && tokio::fs::metadata(out)
            .await
            .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(Error::BundleOutDirectory(out.display().to_string()));
    }
    let explicit = sqlite.is_some();
    let target = resolve(env, sqlite, Kind::Either)?;
    let author = exporter_from(env, explicit).await?;
    let named = (!wanted.is_empty()).then_some(wanted);
    let sessions = read(&target, &author, named.as_deref(), &filter).await?;
    if let Some(wanted) = &named
        && !missing(wanted, &sessions).is_empty()
    {
        let everyone = read(
            &target,
            &author,
            named.as_deref(),
            &BundleFilter {
                owners: OwnerScope::All,
                ..BundleFilter::default()
            },
        )
        .await?;
        return Err(refusal(wanted, &sessions, &everyone, &filter, &author.user));
    }
    let owners: Vec<String> = sessions
        .iter()
        .map(|session| session.origin.author.clone())
        .collect();
    let sessions: Vec<BundledSession> = sessions
        .into_iter()
        .map(BundledSession::forwarding_stored_origin)
        .collect();
    let count = sessions.len();
    let noun = if count == 1 { "session" } else { "sessions" };
    if dry_run {
        let owned: Vec<BundledSession> = sessions
            .into_iter()
            .zip(owners)
            .map(|(mut session, owner)| {
                session.origin.author = owner;
                session
            })
            .collect();
        let shown: Vec<&BundledSession> = owned.iter().collect();
        let mut lines: Vec<String> = shown.iter().map(|session| listed(session)).collect();
        lines.push(format!(
            "would bundle {}; nothing written\n",
            sessions_from(&shown)
        ));
        return Ok(Report::success(lines.concat()));
    }
    let Some(out) = out else {
        return Err(Error::OutRequired);
    };
    let mut text =
        capture::bundle::seal(author, Timestamp::now(), Sanitization::Raw, sessions)?.text;
    text.push(b'\n');
    capture::share::write_file(out.clone(), text, capture::share::Clobber::Replace).await?;
    Ok(Report::success(format!(
        "bundled {count} {noun} (raw, not sanitized) to {}\n",
        out.display()
    )))
}

async fn read(
    target: &Target,
    author: &BundleAuthor,
    named: Option<&[String]>,
    filter: &BundleFilter,
) -> Result<Vec<BundledSession>, Error> {
    Ok(match target {
        Target::Postgres(url) => {
            let ids = named.map(conversation_ids).transpose()?;
            PostgresMemory::connect(url)
                .await?
                .bundled_sessions(author, ids.as_deref(), filter)
                .await?
        }
        Target::Sqlite(path) => {
            open_sqlite_read_only(path)
                .await?
                .bundled_sessions(
                    author.clone(),
                    named.map(<[String]>::to_vec),
                    filter.clone(),
                )
                .await?
        }
    })
}

fn conversation_ids(wanted: &[String]) -> Result<Vec<Uuid>, Error> {
    wanted
        .iter()
        .map(|id| Uuid::parse_str(id).map_err(|_| Error::BundleUnknown(id.clone())))
        .collect()
}

fn missing<'a>(wanted: &'a [String], sessions: &[BundledSession]) -> Vec<&'a str> {
    wanted
        .iter()
        .filter(|id| {
            !sessions
                .iter()
                .any(|session| session.origin.conversation_id.eq_ignore_ascii_case(id))
        })
        .map(String::as_str)
        .collect()
}

fn refusal(
    wanted: &[String],
    sessions: &[BundledSession],
    everyone: &[BundledSession],
    filter: &BundleFilter,
    node_owner: &str,
) -> Error {
    let absent = missing(wanted, sessions);
    let elsewhere = absent.iter().find_map(|id| {
        everyone
            .iter()
            .find(|session| session.origin.conversation_id.eq_ignore_ascii_case(id))
            .map(|session| (*id, session.origin.author.clone()))
    });
    match elsewhere {
        Some((id, owner)) => Error::BundleWrongOwner {
            id: id.to_owned(),
            exporting: filter.owner_list(node_owner).unwrap_or_default().join(", "),
            owner,
        },
        None => Error::BundleUnknown(absent.join(", ")),
    }
}

pub(super) async fn exporter_from(
    env: &Environment,
    explicit: bool,
) -> Result<BundleAuthor, Error> {
    let config = super::target::load_for(env, explicit)?.unwrap_or_default();
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

pub(super) async fn node_owner(config: &Config, env: &Environment) -> Result<String, Error> {
    let path = config.users_file(env);
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
