use std::path::PathBuf;

use super::parse::MemoryCommand;
use super::parse::REQUEUE_DEFAULT;
use super::parse::RequeueCommand;
use super::parse::RetryCommand;
use super::parse::allowed_task;
use super::parse::nonempty;
use super::parse::positive_limit;
use super::parse::reject;
use super::parse::unbounded;
use crate::Error;
use crate::argv::Scan;
use crate::argv::Taken;

pub(super) fn queue_status(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut json = false;
    let mut sqlite = None;
    while scan.current().is_some() {
        if let Some(path) = sqlite_flag(&mut scan)? {
            sqlite = Some(path);
            continue;
        }
        if scan.flag("--json") {
            json = true;
            continue;
        }
        return Err(reject(&scan));
    }
    Ok(MemoryCommand::QueueStatus { json, sqlite })
}

pub(super) fn retry(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut tasks = Vec::new();
    let mut error = None;
    let mut limit = None;
    let mut dry_run = false;
    let mut json = false;
    let mut sqlite = None;
    while scan.current().is_some() {
        if let Some(path) = sqlite_flag(&mut scan)? {
            sqlite = Some(path);
            continue;
        }
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
    Ok(MemoryCommand::Retry(RetryCommand {
        tasks,
        error,
        limit,
        dry_run,
        json,
        sqlite,
    }))
}

pub(super) fn requeue(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut task = None;
    let mut limit = None;
    let mut clamped = false;
    let mut dry_run = false;
    let mut json = false;
    let mut sqlite = None;
    while scan.current().is_some() {
        if let Some(path) = sqlite_flag(&mut scan)? {
            sqlite = Some(path);
            continue;
        }
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
    Ok(MemoryCommand::Requeue(RequeueCommand {
        task,
        limit,
        clamped,
        dry_run,
        json,
        sqlite,
    }))
}

pub(super) fn consolidate_wiki(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut limit = None;
    let mut dry_run = false;
    let mut json = false;
    let mut sqlite = None;
    while scan.current().is_some() {
        if let Some(path) = sqlite_flag(&mut scan)? {
            sqlite = Some(path);
            continue;
        }
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
        sqlite,
    })
}

fn sqlite_flag(scan: &mut Scan<'_>) -> Result<Option<PathBuf>, Error> {
    match scan.option("--sqlite", None) {
        Taken::Value(value) => Ok(Some(nonempty(&value, Error::SqliteRequired)?)),
        Taken::Missing => Err(Error::SqliteRequired),
        Taken::Absent => Ok(None),
    }
}
