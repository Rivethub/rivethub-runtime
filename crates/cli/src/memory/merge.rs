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
    from: String,
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
        } else if let Some(text) = value(&mut scan, "--owner")? {
            owner = Some(Owner::new(text).map_err(|error| invalid(error.to_string()))?);
        } else if let Some(text) = value(&mut scan, "--moves")? {
            moves = Some(PathBuf::from(text));
        } else {
            return Err(Error::UnknownOption(current));
        }
    }
    Ok(MergeArgs {
        from: from.ok_or_else(|| invalid("--from <postgres url> names the store to merge in"))?,
        into,
        owner: owner.ok_or_else(|| invalid("--owner <user> names whose rows they are"))?,
        moves,
        dry_run,
        json,
    })
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
    let into = match args.into {
        Some(url) => url,
        None => super::run::postgres_url(env)?,
    };
    let moves = match &args.moves {
        Some(path) => moves(&tokio::fs::read_to_string(path).await?)?,
        None => BTreeMap::new(),
    };
    let source = PostgresMemory::connect(&args.from).await?;
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
