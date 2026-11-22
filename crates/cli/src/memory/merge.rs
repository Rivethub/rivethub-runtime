use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use config::Environment;
use kernel::owner::Owner;
use memory_postgres::PostgresMemory;
use memory_postgres::merge::MergeReport;
use memory_postgres::merge::MergeRequest;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::Error;
use crate::Report;
use crate::argv::Scan;
use crate::argv::Taken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MergeArgs {
    from: Option<String>,
    from_sqlite: Option<PathBuf>,
    into_sqlite: Option<PathBuf>,
    sqlite_owner: Option<Owner>,
    destination_index_key: Option<[u8; 32]>,
    into: Option<String>,
    owner: Owner,
    moves: Option<PathBuf>,
    dry_run: bool,
    json: bool,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::MergeStore(message.into())
}

fn value(scan: &mut Scan<'_>, name: &str) -> Result<Option<String>, Error> {
    match scan.option(name, None) {
        Taken::Value(value) if !value.trim().is_empty() => Ok(Some(value)),
        Taken::Value(_) | Taken::Missing => Err(invalid(format!("{name} requires a value"))),
        Taken::Absent => Ok(None),
    }
}

pub(super) fn parse(args: &[String]) -> Result<MergeArgs, Error> {
    let mut scan = Scan::new(args);
    let (mut from, mut into, mut owner, mut moves) = (None, None, None, None);
    let (mut from_sqlite, mut into_sqlite, mut sqlite_owner) = (None, None, None);
    let mut destination_index_key = None;
    let (mut dry_run, mut json) = (false, false);
    while let Some(current) = scan.current().map(str::to_owned) {
        if scan.flag("--dry-run") {
            dry_run = true;
        } else if scan.flag("--json") {
            json = true;
        } else if let Some(text) = value(&mut scan, "--from")? {
            from = Some(text);
        } else if let Some(text) = value(&mut scan, "--into")? {
            into = Some(text);
        } else if let Some(text) = value(&mut scan, "--from-sqlite")? {
            from_sqlite = Some(PathBuf::from(text));
        } else if let Some(text) = value(&mut scan, "--into-sqlite")? {
            into_sqlite = Some(PathBuf::from(text));
        } else if let Some(text) = value(&mut scan, "--sqlite-owner")? {
            sqlite_owner = Some(Owner::new(text).map_err(|error| invalid(error.to_string()))?);
        } else if let Some(text) = value(&mut scan, "--destination-index-key")? {
            destination_index_key = Some(parse_index_key(&text)?);
        } else if let Some(text) = value(&mut scan, "--owner")? {
            owner = Some(Owner::new(text).map_err(|error| invalid(error.to_string()))?);
        } else if let Some(text) = value(&mut scan, "--moves")? {
            moves = Some(PathBuf::from(text));
        } else {
            return Err(Error::UnknownOption(current));
        }
    }
    if from.is_some() == from_sqlite.is_some() {
        return Err(invalid(
            "specify exactly one of --from <postgres url> and --from-sqlite <file>",
        ));
    }
    if into_sqlite.is_some() && (into.is_some() || from_sqlite.is_some()) {
        return Err(invalid(
            "--into-sqlite requires a Postgres source and excludes --into",
        ));
    }
    if moves.is_some() && (from_sqlite.is_some() || into_sqlite.is_some()) {
        return Err(invalid(
            "--moves is available for Postgres-to-Postgres merges",
        ));
    }
    if sqlite_owner.is_some() && from_sqlite.is_none() && into_sqlite.is_none() {
        return Err(invalid("--sqlite-owner requires a SQLite endpoint"));
    }
    if destination_index_key.is_some() && from_sqlite.is_none() && into_sqlite.is_none() {
        return Err(invalid(
            "--destination-index-key requires cross-backend transfer",
        ));
    }
    Ok(MergeArgs {
        from,
        from_sqlite,
        into_sqlite,
        sqlite_owner,
        destination_index_key,
        into,
        owner: owner.ok_or_else(|| invalid("--owner <user> names whose rows they are"))?,
        moves,
        dry_run,
        json,
    })
}

fn parse_index_key(text: &str) -> Result<[u8; 32], Error> {
    let mut key = [0; 32];
    hex::decode_to_slice(text, &mut key)
        .map_err(|error| invalid(format!("invalid --destination-index-key: {error}")))?;
    Ok(key)
}

fn moves(text: &str) -> Result<BTreeMap<Uuid, Owner>, Error> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut words = line.split_whitespace();
            let (Some(id), Some(owner), None) = (words.next(), words.next(), words.next()) else {
                return Err(invalid(format!(
                    "a moves line is `<conversation id> <user>`: {line}"
                )));
            };
            let id = Uuid::from_str(id).map_err(|error| invalid(format!("{id}: {error}")))?;
            let owner = Owner::new(owner.to_owned()).map_err(|error| invalid(error.to_string()))?;
            Ok((id, owner))
        })
        .collect()
}

fn lines(report: &MergeReport) -> String {
    let mut out = vec![format!(
        "merge-store {} (run {}): {} in {} ms",
        report.source,
        report.runs,
        if report.committed {
            "committed"
        } else {
            "dry run, rolled back"
        },
        report.millis
    )];
    for ((table, staged), inserted) in report.staged.iter().zip(report.inserted.values()) {
        if *staged > 0 || *inserted > 0 {
            out.push(format!("  {table}: {staged} read, {inserted} added"));
        }
    }
    out.push(format!(
        "  sessions in both stores: {} kept the family copy, {} took the larger merged copy",
        report.kept_family.len(),
        report.replaced.len()
    ));
    out.push(format!(
        "  moved conversations: {}; type topics joined: {}; topics renamed: {}; aliases dropped: {}; wiki pages prefixed: {}",
        report.moved_conversations,
        report.mapped_type_topics,
        report.renamed_topics,
        report.dropped_aliases,
        report.prefixed_pages
    ));
    for tree in &report.dropped_summary_trees {
        out.push(format!(
            "  {}: skipped incoming summary tree {}. Retry or summarize the destination again.",
            tree.session_key, tree.incoming_session
        ));
    }
    if report.dropped_legacy_summaries > 0 {
        out.push(format!(
            "  skipped {} incoming legacy summaries",
            report.dropped_legacy_summaries
        ));
    }
    if report.embeddings_cleared {
        out.push(
            "  incompatible embeddings cleared: affected rows wait to be embedded again".to_owned(),
        );
    }
    out.push(String::new());
    out.join("\n")
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MergeJson<'a> {
    source: &'a str,
    runs: u32,
    committed: bool,
    millis: u64,
    read: &'a BTreeMap<String, u64>,
    added: &'a BTreeMap<String, u64>,
    kept_family: &'a [String],
    replaced: &'a [String],
    dropped_summary_trees: &'a [memory_postgres::merge::DroppedSummaryTree],
    dropped_legacy_summaries: u64,
    moved_conversations: u64,
    joined_type_topics: u64,
    renamed_topics: u64,
    dropped_aliases: u64,
    prefixed_pages: u64,
    embeddings_cleared: bool,
}

fn json(report: &MergeReport) -> Result<String, Error> {
    serde_json::to_string(&MergeJson {
        source: &report.source,
        runs: report.runs,
        committed: report.committed,
        millis: report.millis,
        read: &report.staged,
        added: &report.inserted,
        kept_family: &report.kept_family,
        replaced: &report.replaced,
        dropped_summary_trees: &report.dropped_summary_trees,
        dropped_legacy_summaries: report.dropped_legacy_summaries,
        moved_conversations: report.moved_conversations,
        joined_type_topics: report.mapped_type_topics,
        renamed_topics: report.renamed_topics,
        dropped_aliases: report.dropped_aliases,
        prefixed_pages: report.prefixed_pages,
        embeddings_cleared: report.embeddings_cleared,
    })
    .map_err(|error| invalid(error.to_string()))
}

pub(super) async fn run(
    args: MergeArgs,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Report, Error> {
    if args.from_sqlite.is_some() || args.into_sqlite.is_some() {
        return transfer(args, env, shutdown).await;
    }
    let into = match args.into {
        Some(url) => url,
        None => super::run::postgres_url(env)?,
    };
    let moves = match &args.moves {
        Some(path) => moves(&tokio::fs::read_to_string(path).await?)?,
        None => BTreeMap::new(),
    };
    let source = PostgresMemory::connect(
        args.from
            .as_deref()
            .ok_or_else(|| invalid("--from is required"))?,
    )
    .await?;
    let family = PostgresMemory::connect(&into).await?;
    let request = MergeRequest {
        owner: args.owner,
        moves,
        dry_run: args.dry_run,
    };
    let report = family.merge_store(&source, &request, shutdown).await?;
    Ok(Report::success(if args.json {
        json(&report)?
    } else {
        lines(&report)
    }))
}

async fn transfer(
    args: MergeArgs,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Report, Error> {
    use super::transfer::Direction;

    let (path, url, direction) = match (args.from_sqlite, args.into_sqlite) {
        (Some(path), None) => {
            let url = match args.into {
                Some(url) => url,
                None => super::run::postgres_url(env)?,
            };
            (path, url, Direction::ToPostgres)
        }
        (None, Some(path)) => (
            path,
            args.from.ok_or_else(|| invalid("--from is required"))?,
            Direction::ToSqlite,
        ),
        (None, None) | (Some(_), Some(_)) => {
            return Err(invalid(
                "cross-backend transfer requires one SQLite endpoint",
            ));
        }
    };
    if direction == Direction::ToSqlite && !args.dry_run && !tokio::fs::try_exists(&path).await? {
        let path = path.clone();
        tokio::task::spawn_blocking(move || migrate::sqlite(&path)).await??;
    }
    let local =
        if direction == Direction::ToSqlite && args.dry_run && !tokio::fs::try_exists(&path).await?
        {
            let dir = tempfile::tempdir()?;
            let path = dir.path().join("memory.sqlite");
            let store = tokio::task::spawn_blocking(move || {
                migrate::sqlite(&path)?;
                Ok::<_, Error>(memory_sqlite::SqliteStore::open(&path)?)
            })
            .await??;
            (store, Some(dir))
        } else {
            (super::run::open_sqlite(&path).await?, None)
        };
    let remote = PostgresMemory::connect(&url).await?;
    let report = super::transfer::run(
        remote.pool(),
        &local.0,
        [
            &args.owner,
            args.sqlite_owner.as_ref().unwrap_or(&args.owner),
        ],
        direction,
        (args.dry_run, args.destination_index_key),
        shutdown,
    )
    .await?;
    Ok(Report::success(if args.json {
        format!("{}\n", serde_json::to_string(&report)?)
    } else {
        let read: u64 = report.read.values().sum();
        let added: u64 = report.added.values().sum();
        let state = if report.committed {
            "committed"
        } else {
            "dry run, rolled back"
        };
        format!(
            "merge-store {state}, {read} rows read, {added} added\nClaims left behind on other people’s topics = {}\nRedirects left behind with another owner = {}\nUnattributed topic tombstones left behind = {}\n{}\n{}\n",
            report.omissions.claims_on_other_topics,
            report.omissions.redirects_owned_by_others,
            report.omissions.unattributed_topic_tombstones,
            report.embeddings,
            report.excluded
        )
    }))
}
