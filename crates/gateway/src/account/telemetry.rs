use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::AccountError;
use super::install::{AccountFiles, write_private};
use super::update::CloudOs;

const FRAME_LIMIT: usize = 160;
const FRAMES: usize = 40;
const QUEUE_LIMIT: usize = 20;
const UNPLACED: &str = "<unplaced>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::Display)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum CrashJob {
    Gateway,
    Capture,
    Embedder,
    Summarizer,
    Categorizer,
    WikiWriter,
    EntityBuilder,
    Mcp,
    Cli,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CrashKind {
    Panic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CrashReport {
    pub schema: u8,
    pub install_id: Uuid,
    pub rh: String,
    pub os: CloudOs,
    pub job: CrashJob,
    pub kind: CrashKind,
    #[serde(rename = "where")]
    pub location: String,
    pub frames: Vec<String>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub schema: u8,
    pub install_id: Uuid,
    pub rh: String,
    pub os: CloudOs,
    pub day: NaiveDate,
    pub counts: UsageCounts,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCounts {
    pub searches: Option<u64>,
    pub app_opens: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub struct PanicSite<'a> {
    pub file: &'a str,
    pub line: u32,
}

impl CrashReport {
    #[must_use]
    pub fn panic(
        install_id: Uuid,
        os: CloudOs,
        job: CrashJob,
        site: Option<PanicSite<'_>>,
        backtrace: &str,
        at: DateTime<Utc>,
    ) -> Self {
        let location = match site {
            Some(site) => frame(&format!("{}:{}", module_of(site.file), site.line)),
            None => UNPLACED.to_owned(),
        };
        Self {
            schema: 1,
            install_id,
            rh: crate::VERSION.to_owned(),
            os,
            job,
            kind: CrashKind::Panic,
            location,
            frames: frames(backtrace),
            at: at.to_rfc3339_opts(SecondsFormat::Millis, true),
        }
    }
}

fn module_of(file: &str) -> String {
    let parts: Vec<&str> = file.split(['/', '\\']).collect();
    let Some(source) = parts
        .iter()
        .rposition(|part| matches!(*part, "src" | "tests" | "benches" | "examples"))
    else {
        return UNPLACED.to_owned();
    };
    let Some(owner) = source.checked_sub(1).and_then(|index| parts.get(index)) else {
        return UNPLACED.to_owned();
    };
    let krate = match owner.rsplit_once('-') {
        Some((name, version)) if version.starts_with(|c: char| c.is_ascii_digit()) => name,
        Some(_) | None => owner,
    };
    let mut module = vec![krate.replace('-', "_")];
    for part in parts.iter().skip(source + 1) {
        let part = part.strip_suffix(".rs").unwrap_or(part);
        if !matches!(part, "mod" | "lib" | "main") {
            module.push(part.to_owned());
        }
    }
    module.join("::")
}

fn frames(backtrace: &str) -> Vec<String> {
    let mut frames: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in backtrace.lines() {
        let text = line.trim();
        if let Some(location) = text.strip_prefix("at ") {
            if let Some(symbol) = current.take() {
                let line = location
                    .rsplit(':')
                    .nth(1)
                    .filter(|line| line.chars().all(|c| c.is_ascii_digit()));
                frames.push(match line {
                    Some(line) => format!("{symbol}:{line}"),
                    None => symbol,
                });
            }
            continue;
        }
        if let Some(symbol) = current.take() {
            frames.push(symbol);
        }
        let Some((index, symbol)) = text.split_once(": ") else {
            continue;
        };
        if index.chars().all(|c| c.is_ascii_digit()) {
            current = symbol_frame(symbol);
        }
    }
    if let Some(symbol) = current {
        frames.push(symbol);
    }
    frames
        .into_iter()
        .map(|text| frame(&text))
        .take(FRAMES)
        .collect()
}

fn symbol_frame(symbol: &str) -> Option<String> {
    let symbol = without_disambiguators(symbol);
    let skipped = symbol.contains(['/', '\\']) || !symbol.contains("::") || machinery(&symbol);
    if skipped {
        return None;
    }
    let symbol = match symbol.rsplit_once("::h") {
        Some((head, hash)) if hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()) => {
            head
        }
        Some(_) | None => &symbol,
    };
    let cleaned = frame(symbol);
    (!cleaned.is_empty()).then_some(cleaned)
}

fn machinery(symbol: &str) -> bool {
    let unwinding = symbol.starts_with("std::panicking")
        || symbol.starts_with("core::panicking")
        || symbol.starts_with("std::panic::")
        || symbol.starts_with("std::backtrace")
        || symbol.starts_with("std::sys")
        || symbol.starts_with("std::rt::");
    let shim = symbol.contains("alloc::boxed::Box")
        || symbol.contains("core::ops::function::Fn")
        || symbol.contains("__rust_");
    unwinding || shim || symbol.starts_with("gateway::account::telemetry")
}

fn without_disambiguators(symbol: &str) -> String {
    let mut kept = String::with_capacity(symbol.len());
    let mut chars = symbol.chars();
    while let Some(c) = chars.next() {
        let close = match c {
            '[' => ']',
            '{' => '}',
            other => {
                kept.push(other);
                continue;
            }
        };
        let inner: String = chars.by_ref().take_while(|next| *next != close).collect();
        let inner = inner.trim_matches(['{', '}']);
        if close == '}' && inner.starts_with("closure") {
            kept.push_str("<closure>");
        } else if close == ']' && !inner.chars().all(|c| c.is_ascii_hexdigit()) {
            kept.push('[');
            kept.push_str(inner);
            kept.push(']');
        }
    }
    kept
}

fn frame(text: &str) -> String {
    let allowed: String = text
        .chars()
        .filter(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '<' | '>' | '.' | '-' | ' ')
        })
        .collect();
    let mut words = allowed.split_whitespace().collect::<Vec<_>>().join(" ");
    words.truncate(FRAME_LIMIT);
    words
}

pub fn install_crash_hook(state_dir: &Path, job: CrashJob) {
    let files = AccountFiles::new(state_dir);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let site = info.location().map(|location| PanicSite {
            file: location.file(),
            line: location.line(),
        });
        if let Err(error) = queue_panic(&files, job, site) {
            tracing::debug!(%error, "the crash report was not queued");
        }
        previous(info);
    }));
}

fn queue_panic(
    files: &AccountFiles,
    job: CrashJob,
    site: Option<PanicSite<'_>>,
) -> Result<Option<PathBuf>, AccountError> {
    let Some(state) = files.read()? else {
        return Ok(None);
    };
    if !state.crash_reports {
        return Ok(None);
    }
    let os = CloudOs::current().ok_or(AccountError::Platform)?;
    let backtrace = std::backtrace::Backtrace::force_capture().to_string();
    let report = CrashReport::panic(state.install_id, os, job, site, &backtrace, Utc::now());
    queue(files, &report)
}

pub fn queue(files: &AccountFiles, report: &CrashReport) -> Result<Option<PathBuf>, AccountError> {
    let folder = files.crashes();
    let waiting = match std::fs::read_dir(&folder) {
        Ok(entries) => entries.count(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.into()),
    };
    if waiting >= QUEUE_LIMIT {
        return Ok(None);
    }
    let path = folder.join(format!("{}.json", Uuid::new_v4()));
    write_private(&path, &serde_json::to_vec(report)?)?;
    Ok(Some(path))
}

pub fn queued(files: &AccountFiles) -> Result<Vec<PathBuf>, AccountError> {
    let entries = match std::fs::read_dir(files.crashes()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}
