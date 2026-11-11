use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;
use kernel::queue::FixedJob;
use kernel::queue::JobName;

use crate::Error;
use crate::argv::Scan;
use crate::argv::Taken;

const ALLOWED_TASKS: &str =
    "extract-wiki, compact-conversation, embed-target, synthesize-tool-call, file-conversation";
const REQUEUE_DEFAULT: u32 = 200;
const REQUEUE_CAP: u32 = 1000;

pub(super) enum MemoryCommand {
    QueueStatus {
        json: bool,
    },
    Retry {
        tasks: Vec<String>,
        error: Option<String>,
        limit: Option<u32>,
        dry_run: bool,
        json: bool,
    },
    Requeue {
        task: JobName,
        limit: u32,
        clamped: bool,
        dry_run: bool,
        json: bool,
    },
    ToolSynth {
        limit: Option<u32>,
        dry_run: bool,
        json: bool,
    },
    Export {
        out: Option<PathBuf>,
        since: Option<DateTime<Utc>>,
        sqlite: Option<PathBuf>,
    },
    Import {
        path: PathBuf,
        dry_run: bool,
        sqlite: Option<PathBuf>,
    },
    Titles {
        dry_run: bool,
        json: bool,
        sqlite: Option<PathBuf>,
        model: Option<super::titles::TitleModel>,
    },
    Reembed {
        dry_run: bool,
        sqlite: Option<PathBuf>,
    },
    ConsolidateWiki {
        limit: Option<u32>,
        dry_run: bool,
        json: bool,
    },
    RecompileWiki(super::recompile::RecompileArgs),
    Cleanup(super::cleanup::CleanupArgs),
    MergeStore(super::merge::MergeArgs),
    Bundle {
        out: PathBuf,
        sessions: Vec<String>,
        sqlite: Option<PathBuf>,
    },
    IngestBundle {
        path: PathBuf,
        force: bool,
        dry_run: bool,
    },
    Sanitize(super::sanitize::Job),
    Share(super::share::ShareCommand),
    Resolve {
        report: PathBuf,
        bundle: Option<PathBuf>,
        mapping: Option<PathBuf>,
        learn: bool,
    },
}

enum Verb {
    QueueStatus,
    RetryFailed,
    Requeue,
    ToolSynth,
    Export,
    Import,
    Titles,
    Reembed,
    ConsolidateWiki,
    RecompileWiki,
    Cleanup,
    MergeStore,
    Bundle,
    IngestBundle,
    Sanitize,
    Share,
    Receive,
    Key,
    Trust,
}

fn verb(text: &str) -> Option<Verb> {
    match text {
        "queue-status" => Some(Verb::QueueStatus),
        "retry-failed" => Some(Verb::RetryFailed),
        "requeue" => Some(Verb::Requeue),
        "backfill-tool-synth" => Some(Verb::ToolSynth),
        "export" => Some(Verb::Export),
        "import" => Some(Verb::Import),
        "backfill-titles" => Some(Verb::Titles),
        "reembed" => Some(Verb::Reembed),
        "consolidate-wiki" => Some(Verb::ConsolidateWiki),
        "recompile-wiki" => Some(Verb::RecompileWiki),
        "cleanup" => Some(Verb::Cleanup),
        "merge-store" => Some(Verb::MergeStore),
        "bundle" => Some(Verb::Bundle),
        "ingest-bundle" => Some(Verb::IngestBundle),
        "sanitize" => Some(Verb::Sanitize),
        "share" => Some(Verb::Share),
        "receive" => Some(Verb::Receive),
        "key" => Some(Verb::Key),
        "trust" => Some(Verb::Trust),
        _ => None,
    }
}

pub(super) fn parse(args: &[String]) -> Result<Option<MemoryCommand>, Error> {
    if args.iter().any(is_help) {
        return Ok(None);
    }
    let Some((head, rest)) = args.split_first() else {
        return Ok(None);
    };
    let Some(verb) = verb(head) else {
        if head.starts_with('-') {
            return Err(Error::UnknownOption(head.clone()));
        }
        return Ok(None);
    };
    match verb {
        Verb::QueueStatus => queue_status(rest).map(Some),
        Verb::RetryFailed => retry(rest).map(Some),
        Verb::Requeue => requeue(rest).map(Some),
        Verb::ToolSynth => tool_synth(rest).map(Some),
        Verb::Export => export(rest).map(Some),
        Verb::Import => import(rest).map(Some),
        Verb::Titles => titles(rest).map(Some),
        Verb::Reembed => reembed(rest).map(Some),
        Verb::ConsolidateWiki => consolidate_wiki(rest).map(Some),
        Verb::RecompileWiki => super::recompile::parse(rest)
            .map(MemoryCommand::RecompileWiki)
            .map(Some),
        Verb::Cleanup => super::cleanup::parse(rest)
            .map(MemoryCommand::Cleanup)
            .map(Some),
        Verb::MergeStore => super::merge::parse(rest)
            .map(MemoryCommand::MergeStore)
            .map(Some),
        Verb::Bundle => super::bundle::args(rest).map(Some),
        Verb::IngestBundle => super::bundle::ingest_args(rest).map(Some),
        Verb::Sanitize => super::sanitize::args(rest).map(Some),
        Verb::Share => super::share::share_args(rest).map(Some),
        Verb::Receive => super::share::receive_args(rest).map(Some),
        Verb::Key => super::share::key_args(rest).map(Some),
        Verb::Trust => super::share::trust_args(rest).map(Some),
    }
}

fn is_help(arg: &String) -> bool {
    arg == "--help" || arg == "-h"
}

fn queue_status(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut json = false;
    while scan.current().is_some() {
        if scan.flag("--json") {
            json = true;
            continue;
        }
        return Err(reject(&scan));
    }
    Ok(MemoryCommand::QueueStatus { json })
}

fn retry(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut tasks = Vec::new();
    let mut error = None;
    let mut limit = None;
    let mut dry_run = false;
    let mut json = false;
    while scan.current().is_some() {
        match scan.option("--task", None) {
            Taken::Value(value) => {
                tasks.push(value);
                continue;
            }
            Taken::Missing => return Err(Error::TaskRequired),
            Taken::Absent => {}
        }
        match scan.option("--error", None) {
            Taken::Value(value) => {
                error = Some(value);
                continue;
            }
            Taken::Missing => return Err(Error::ErrorSubstringRequired),
            Taken::Absent => {}
        }
        match scan.option("--limit", None) {
            Taken::Value(value) => {
                limit = unbounded(&value)?;
                continue;
            }
            Taken::Missing => return Err(Error::LimitInteger),
            Taken::Absent => {}
        }
        if scan.flag("--dry-run") {
            dry_run = true;
            continue;
        }
        if scan.flag("--json") {
            json = true;
            continue;
        }
        return Err(reject(&scan));
    }
    if tasks.is_empty() {
        return Err(Error::RetryTaskFilter);
    }
    Ok(MemoryCommand::Retry {
        tasks,
        error,
        limit,
        dry_run,
        json,
    })
}

fn requeue(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut task = None;
    let mut limit = None;
    let mut clamped = false;
    let mut dry_run = false;
    let mut json = false;
    while scan.current().is_some() {
        match scan.option("--task", None) {
            Taken::Value(value) => {
                if task.is_some() {
                    return Err(Error::OneTask);
                }
                task = Some(allowed_task(&value)?);
                continue;
            }
            Taken::Missing => return Err(Error::TaskRequired),
            Taken::Absent => {}
        }
        match scan.option("--limit", None) {
            Taken::Value(value) => {
                let (parsed, hit_cap) = positive_limit(&value)?;
                limit = Some(parsed);
                clamped = hit_cap;
                continue;
            }
            Taken::Missing => return Err(Error::LimitInteger),
            Taken::Absent => {}
        }
        if scan.flag("--dry-run") {
            dry_run = true;
            continue;
        }
        if scan.flag("--json") {
            json = true;
            continue;
        }
        return Err(reject(&scan));
    }
    let Some(task) = task else {
        return Err(Error::RequeueTaskFilter);
    };
    let limit = match limit {
        Some(limit) => limit,
        None => REQUEUE_DEFAULT,
    };
    Ok(MemoryCommand::Requeue {
        task,
        limit,
        clamped,
        dry_run,
        json,
    })
}

fn tool_synth(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut limit = None;
    let mut dry_run = false;
    let mut json = false;
    while scan.current().is_some() {
        match scan.option("--limit", None) {
            Taken::Value(value) => {
                limit = unbounded(&value)?;
                continue;
            }
            Taken::Missing => return Err(Error::LimitInteger),
            Taken::Absent => {}
        }
        if scan.flag("--dry-run") {
            dry_run = true;
            continue;
        }
        if scan.flag("--json") {
            json = true;
            continue;
        }
        return Err(reject(&scan));
    }
    Ok(MemoryCommand::ToolSynth {
        limit,
        dry_run,
        json,
    })
}

fn export(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut out = None;
    let mut since = None;
    let mut sqlite = None;
    while scan.current().is_some() {
        match scan.option("--out", None) {
            Taken::Value(value) => {
                out = Some(nonempty(&value, Error::OutRequired)?);
                continue;
            }
            Taken::Missing => return Err(Error::OutRequired),
            Taken::Absent => {}
        }
        match scan.option("--since", None) {
            Taken::Value(value) => {
                since = Some(timestamp(&value)?);
                continue;
            }
            Taken::Missing => return Err(Error::SinceRequired),
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
    Ok(MemoryCommand::Export { out, since, sqlite })
}

fn import(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut path = None;
    let mut dry_run = false;
    let mut sqlite = None;
    while scan.current().is_some() {
        match scan.option("--sqlite", None) {
            Taken::Value(value) => {
                sqlite = Some(nonempty(&value, Error::SqliteRequired)?);
                continue;
            }
            Taken::Missing => return Err(Error::SqliteRequired),
            Taken::Absent => {}
        }
        if scan.flag("--dry-run") {
            dry_run = true;
            continue;
        }
        let Some(token) = scan.current().map(str::to_owned) else {
            break;
        };
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
    Ok(MemoryCommand::Import {
        path,
        dry_run,
        sqlite,
    })
}

fn titles(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut dry_run = false;
    let mut json = false;
    let mut sqlite = None;
    let (mut model, mut model_url, mut concurrency) = (None, None, None);
    while scan.current().is_some() {
        match scan.option("--sqlite", None) {
            Taken::Value(value) => {
                sqlite = Some(nonempty(&value, Error::SqliteRequired)?);
                continue;
            }
            Taken::Missing => return Err(Error::SqliteRequired),
            Taken::Absent => {}
        }
        match scan.option("--model", None) {
            Taken::Value(value) => {
                model = Some(value);
                continue;
            }
            Taken::Missing => return Err(Error::TitlesModel),
            Taken::Absent => {}
        }
        match scan.option("--model-url", None) {
            Taken::Value(value) => {
                model_url = Some(value);
                continue;
            }
            Taken::Missing => return Err(Error::TitlesModel),
            Taken::Absent => {}
        }
        match scan.option("--concurrency", None) {
            Taken::Value(value) => {
                concurrency = Some(positive_count(&value)?);
                continue;
            }
            Taken::Missing => return Err(Error::CleanupNumber("--concurrency")),
            Taken::Absent => {}
        }
        if scan.flag("--dry-run") {
            dry_run = true;
            continue;
        }
        if scan.flag("--json") {
            json = true;
            continue;
        }
        return Err(reject(&scan));
    }
    let model = match (model, model_url) {
        (Some(name), Some(url)) => Some(super::titles::TitleModel::new(name, url, concurrency)),
        (None, None) if concurrency.is_none() => None,
        _ => return Err(Error::TitlesModel),
    };
    Ok(MemoryCommand::Titles {
        dry_run,
        json,
        sqlite,
        model,
    })
}

fn reembed(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut dry_run = false;
    let mut sqlite = None;
    while scan.current().is_some() {
        match scan.option("--sqlite", None) {
            Taken::Value(value) => {
                sqlite = Some(nonempty(&value, Error::SqliteRequired)?);
                continue;
            }
            Taken::Missing => return Err(Error::SqliteRequired),
            Taken::Absent => {}
        }
        if scan.flag("--dry-run") {
            dry_run = true;
            continue;
        }
        return Err(reject(&scan));
    }
    Ok(MemoryCommand::Reembed { dry_run, sqlite })
}

fn consolidate_wiki(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut limit = None;
    let mut dry_run = false;
    let mut json = false;
    while scan.current().is_some() {
        match scan.option("--limit", None) {
            Taken::Value(value) => {
                limit = Some(positive_limit(&value)?.0);
                continue;
            }
            Taken::Missing => return Err(Error::LimitInteger),
            Taken::Absent => {}
        }
        if scan.flag("--dry-run") {
            dry_run = true;
            continue;
        }
        if scan.flag("--json") {
            json = true;
            continue;
        }
        return Err(reject(&scan));
    }
    Ok(MemoryCommand::ConsolidateWiki {
        limit,
        dry_run,
        json,
    })
}

pub(super) fn reject(scan: &Scan<'_>) -> Error {
    match scan.current() {
        Some(token) if token.starts_with('-') => Error::UnknownOption(token.to_owned()),
        Some(token) => Error::Unexpected(token.to_owned()),
        None => Error::Unexpected(String::new()),
    }
}

fn allowed_task(text: &str) -> Result<JobName, Error> {
    let task = match text {
        "extract-wiki" => FixedJob::ExtractWiki,
        "compact-conversation" => FixedJob::SummarizeConversation,
        "embed-target" => FixedJob::EmbedTarget,
        "synthesize-tool-call" => FixedJob::SynthesizeToolCall,
        "file-conversation" => FixedJob::FileConversation,
        _ => {
            return Err(Error::TaskNotAllowed {
                task: text.to_owned(),
                allowed: ALLOWED_TASKS,
            });
        }
    };
    Ok(JobName::Fixed(task))
}

fn unbounded(text: &str) -> Result<Option<u32>, Error> {
    let parsed = text.parse::<i64>().map_err(|_| Error::LimitInteger)?;
    if parsed <= 0 {
        return Ok(None);
    }
    u32::try_from(parsed)
        .map(Some)
        .map_err(|_| Error::LimitInteger)
}

fn positive_limit(text: &str) -> Result<(u32, bool), Error> {
    let parsed = text.parse::<i64>().map_err(|_| Error::LimitInteger)?;
    if parsed <= 0 {
        return Err(Error::LimitPositive);
    }
    if parsed > i64::from(REQUEUE_CAP) {
        return Ok((REQUEUE_CAP, true));
    }
    Ok((
        u32::try_from(parsed).map_err(|_| Error::LimitInteger)?,
        false,
    ))
}

fn positive_count(text: &str) -> Result<usize, Error> {
    match text.parse::<usize>() {
        Ok(count) if count > 0 => Ok(count),
        Ok(_) | Err(_) => Err(Error::CleanupNumber("--concurrency")),
    }
}

fn timestamp(text: &str) -> Result<DateTime<Utc>, Error> {
    DateTime::parse_from_rfc3339(text)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| Error::SinceInvalid(text.to_owned()))
}

pub(super) fn nonempty(text: &str, missing: Error) -> Result<PathBuf, Error> {
    if text.is_empty() {
        Err(missing)
    } else {
        Ok(PathBuf::from(text))
    }
}
