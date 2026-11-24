use jiff::Timestamp;
use std::path::PathBuf;

use kernel::queue::FixedJob;
use kernel::queue::JobName;

use crate::Error;
use crate::argv::Scan;
use crate::argv::Taken;

const ALLOWED_TASKS: &str =
    "extract-wiki, compact-conversation, embed-target, synthesize-tool-call, file-conversation";
pub(super) const REQUEUE_DEFAULT: u32 = 200;
const REQUEUE_CAP: u32 = 1000;

pub(super) struct RetryCommand {
    pub(super) tasks: Vec<String>,
    pub(super) error: Option<String>,
    pub(super) limit: Option<u32>,
    pub(super) dry_run: bool,
    pub(super) json: bool,
    pub(super) sqlite: Option<PathBuf>,
}

pub(super) struct RequeueCommand {
    pub(super) task: JobName,
    pub(super) limit: u32,
    pub(super) clamped: bool,
    pub(super) dry_run: bool,
    pub(super) json: bool,
    pub(super) sqlite: Option<PathBuf>,
}

pub(super) enum MemoryCommand {
    Settle(super::settle::Args),
    Resummarize(super::resummarize::Args),
    Pause(super::pause::Args),
    QueueStatus {
        json: bool,
        sqlite: Option<PathBuf>,
    },
    Retry(RetryCommand),
    Requeue(RequeueCommand),
    ToolSynth {
        limit: Option<u32>,
        dry_run: bool,
        json: bool,
        sqlite: Option<PathBuf>,
    },
    Export {
        out: Option<PathBuf>,
        since: Option<Timestamp>,
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
        sqlite: Option<PathBuf>,
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
        decisions: capture::receive::Decisions,
        gateway: Option<String>,
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
    Settle,
    Resummarize,
    Pause,
    Resume,
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
        "settle" => Some(Verb::Settle),
        "resummarize" => Some(Verb::Resummarize),
        "pause" => Some(Verb::Pause),
        "resume" => Some(Verb::Resume),
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
        Verb::Settle => super::settle::parse(rest)
            .map(MemoryCommand::Settle)
            .map(Some),
        Verb::Resummarize => super::resummarize::parse(rest)
            .map(MemoryCommand::Resummarize)
            .map(Some),
        Verb::Pause => super::pause::parse_pause(rest)
            .map(MemoryCommand::Pause)
            .map(Some),
        Verb::Resume => super::pause::parse_resume(rest)
            .map(MemoryCommand::Pause)
            .map(Some),
        Verb::QueueStatus => super::maintain::queue_status(rest).map(Some),
        Verb::RetryFailed => super::maintain::retry(rest).map(Some),
        Verb::Requeue => super::maintain::requeue(rest).map(Some),
        Verb::ToolSynth => tool_synth(rest).map(Some),
        Verb::Export => export(rest).map(Some),
        Verb::Import => import(rest).map(Some),
        Verb::Titles => titles(rest).map(Some),
        Verb::Reembed => reembed(rest).map(Some),
        Verb::ConsolidateWiki => super::maintain::consolidate_wiki(rest).map(Some),
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

fn tool_synth(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut limit = None;
    let mut dry_run = false;
    let mut json = false;
    let mut sqlite = None;
    while scan.current().is_some() {
        match scan.option("--limit", None) {
            Taken::Value(value) => {
                limit = unbounded(&value)?;
                continue;
            }
            Taken::Missing => return Err(Error::LimitInteger),
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
        sqlite,
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

pub(super) fn reject(scan: &Scan<'_>) -> Error {
    match scan.current() {
        Some(token) if token.starts_with('-') => Error::UnknownOption(token.to_owned()),
        Some(token) => Error::Unexpected(token.to_owned()),
        None => Error::Unexpected(String::new()),
    }
}

pub(super) fn allowed_task(text: &str) -> Result<JobName, Error> {
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

pub(super) fn unbounded(text: &str) -> Result<Option<u32>, Error> {
    let parsed = text.parse::<i64>().map_err(|_| Error::LimitInteger)?;
    if parsed <= 0 {
        return Ok(None);
    }
    u32::try_from(parsed)
        .map(Some)
        .map_err(|_| Error::LimitInteger)
}

pub(super) fn positive_limit(text: &str) -> Result<(u32, bool), Error> {
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

fn timestamp(text: &str) -> Result<Timestamp, Error> {
    wire::time::parse_rfc3339(text).map_err(|_| Error::SinceInvalid(text.to_owned()))
}

pub(super) fn nonempty(text: &str, missing: Error) -> Result<PathBuf, Error> {
    if text.is_empty() {
        Err(missing)
    } else {
        Ok(PathBuf::from(text))
    }
}
