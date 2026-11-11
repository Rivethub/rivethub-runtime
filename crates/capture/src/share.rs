use std::collections::BTreeMap;
use std::io::Write as _;
use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;
use config::Config;
use config::Environment;
use config::MappingFile;
use kernel::memory::share::Choice;
use kernel::memory::share::SessionRedaction;
use thiserror::Error;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::LOCAL_SUFFIX;
use wire::capture::REPORT_SUFFIX;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;

use crate::bundle::BundleError;
use crate::sanitize::KeyError;
use crate::sanitize::LocalKey;
use crate::sanitize::Policy;
use crate::sanitize::Report;
use crate::sanitize::SanitizeError;
use crate::sanitize::SessionRules;
use crate::sanitize::decide::DecideError;
use crate::sanitize::decide::Endpoint;
use crate::sanitize::decide::Mode;
use crate::sanitize::decide::Review;
use crate::sanitize::mapping::Mapping;
use crate::sanitize::mapping::MappingError;
use crate::sanitize::marks::ReportError;

const MAX_CONCURRENCY: usize = 8;

#[derive(Debug, Error)]
pub enum ShareError {
    #[error("the mapping file {path} is invalid: {source}")]
    Mapping { path: String, source: MappingError },
    #[error("the mapping file {path} could not be read: {source}")]
    MappingRead {
        path: String,
        source: std::io::Error,
    },
    #[error("the redact terms do not build a search: {0}")]
    SessionTerms(MappingError),
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error(transparent)]
    Setup(#[from] SetupError),
    #[error(transparent)]
    Key(#[from] KeyError),
    #[error(transparent)]
    Sanitize(#[from] SanitizeError),
    #[error("the decision pass failed, so no bundle was written: {0}")]
    Decide(#[from] DecideError),
    #[error("the report sanitize wrote does not read back: {0}")]
    Report(ReportError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum SetupError {
    #[error(
        "--decide needs memory.sanitize.decide.url in the config (the decision model, for example http://<host>:8015)"
    )]
    Unconfigured,
    #[error("memory.sanitize.decide.threshold is {0}; set it above 0 and at most 1")]
    Threshold(f64),
    #[error(
        "memory.sanitize.decide.concurrency is {0}; set it from 1 to 8, since the decision model is a shared server"
    )]
    Concurrency(usize),
    #[error("memory.sanitize.decide.top is 0; set it to 1 or more")]
    Top,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundlePaths {
    pub bundle: PathBuf,
    pub report: PathBuf,
    pub local: PathBuf,
}

impl BundlePaths {
    #[must_use]
    pub fn beside(out: &Path) -> Self {
        let base = if out.extension().is_some_and(|extension| extension == "json") {
            out.with_extension("")
        } else {
            out.to_path_buf()
        };
        let mut name = base.into_os_string();
        name.push(REPORT_SUFFIX);
        let report = PathBuf::from(name);
        Self {
            bundle: out.to_path_buf(),
            local: local_beside(&report),
            report,
        }
    }
}

#[must_use]
pub fn local_beside(report: &Path) -> PathBuf {
    let name = report.display().to_string();
    let stem = name.strip_suffix(REPORT_SUFFIX).unwrap_or(&name);
    PathBuf::from(format!("{stem}{LOCAL_SUFFIX}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clobber {
    Replace,
    Refuse,
}

pub async fn write_file(path: PathBuf, bytes: Vec<u8>, clobber: Clobber) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || {
        let parent = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            Some(_) | None => Path::new("."),
        };
        let mut partial = tempfile::Builder::new()
            .prefix(".rh-bundle-")
            .suffix(".tmp")
            .tempfile_in(parent)?;
        partial.write_all(&bytes)?;
        match clobber {
            Clobber::Replace => partial.persist(&path),
            Clobber::Refuse => partial.persist_noclobber(&path),
        }
        .map_err(|error| error.error)?;
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)?
}

#[derive(Debug, Clone)]
pub struct Decider {
    pub endpoint: Endpoint,
    pub review: Review,
}

pub fn decider(config: &Config, block: bool) -> Result<Decider, SetupError> {
    let section = config
        .sanitize_decide()
        .filter(|section| !section.url.trim().is_empty())
        .ok_or(SetupError::Unconfigured)?;
    if !(section.threshold > 0.0 && section.threshold <= 1.0) {
        return Err(SetupError::Threshold(section.threshold));
    }
    let concurrency = NonZeroUsize::new(section.concurrency)
        .filter(|concurrency| concurrency.get() <= MAX_CONCURRENCY)
        .ok_or(SetupError::Concurrency(section.concurrency))?;
    Ok(Decider {
        endpoint: Endpoint {
            url: section.url.trim().to_owned(),
            concurrency,
        },
        review: Review {
            top: NonZeroUsize::new(section.top).ok_or(SetupError::Top)?,
            threshold: section.threshold,
            mode: if block || section.block {
                Mode::Block
            } else {
                Mode::Advisory
            },
        },
    })
}

pub fn key_path(env: &Environment) -> Result<PathBuf, config::ConfigError> {
    let state = match env.text("XDG_STATE_HOME").filter(|text| !text.is_empty()) {
        Some(text) => PathBuf::from(text),
        None => config::Paths::new(env)?.home().join(".local").join("state"),
    };
    Ok(LocalKey::path_in(&state))
}

#[derive(Debug)]
pub struct Job<'a> {
    pub bundle: &'a SessionBundle,
    pub sessions: &'a BTreeMap<usize, SessionRedaction>,
    pub mapping: &'a MappingFile,
    pub key: &'a Path,
    pub decider: Option<&'a Decider>,
    pub paths: &'a BundlePaths,
    pub clobber: Clobber,
    pub at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct Outcome {
    pub report: Report,
    pub sanitized: SessionBundle,
    pub local: bool,
    pub mark_lines: BTreeMap<usize, NonZeroUsize>,
}

pub async fn sanitize(job: Job<'_>) -> Result<Outcome, ShareError> {
    let (mapping, source) = rules(job.mapping).await?;
    let key = LocalKey::load_or_create(job.key)?;
    let policy = Policy {
        mapping,
        key,
        sessions: SessionRules::new(job.sessions).map_err(ShareError::SessionTerms)?,
    };
    let mut scrubbed = crate::sanitize::scrub(job.bundle, &policy)?;
    if let Some(decider) = job.decider {
        let candidates = scrubbed.candidates();
        let scores = crate::sanitize::decide::score(&decider.endpoint, &candidates.chunks).await?;
        scrubbed.flag(candidates, &scores, decider.review)?;
    }
    let sanitized = scrubbed.seal(&policy, job.at)?;
    let paths = job.paths;
    let markdown = sanitized
        .report
        .markdown(&paths.bundle.display().to_string(), &source);
    let mark_lines = match &sanitized.report.decision {
        Some(_) => crate::sanitize::marks::read_flags(&markdown)
            .map_err(ShareError::Report)?
            .flags
            .iter()
            .map(|flag| (flag.number, flag.mark_line()))
            .collect(),
        None => BTreeMap::new(),
    };
    write_file(paths.report.clone(), markdown.into_bytes(), job.clobber).await?;
    if let Some(local) = &sanitized.local {
        write_file(paths.local.clone(), serde_json::to_vec(local)?, job.clobber).await?;
    }
    let mut text = sanitized.sealed.text;
    text.push(b'\n');
    write_file(paths.bundle.clone(), text, job.clobber).await?;
    Ok(Outcome {
        report: sanitized.report,
        sanitized: sanitized.sealed.bundle,
        local: sanitized.local.is_some(),
        mark_lines,
    })
}

#[derive(Debug)]
pub struct Chosen {
    pub bundle: SessionBundle,
    pub sessions: BTreeMap<usize, SessionRedaction>,
}

pub fn chosen(
    author: BundleAuthor,
    sessions: Vec<(BundledSession, Choice)>,
    at: DateTime<Utc>,
) -> Result<Chosen, BundleError> {
    let mut rules = BTreeMap::new();
    let mut kept = Vec::with_capacity(sessions.len());
    for (index, (session, choice)) in sessions.into_iter().enumerate() {
        if let Choice::Redact(redaction) = choice {
            rules.insert(index, redaction);
        }
        kept.push(session);
    }
    Ok(Chosen {
        bundle: crate::bundle::seal(author, at, Sanitization::Raw, kept)?.bundle,
        sessions: rules,
    })
}

async fn rules(mapping: &MappingFile) -> Result<(Mapping, String), ShareError> {
    let (path, required) = match mapping {
        MappingFile::Configured(path) => (path, true),
        MappingFile::Default(path) => (path, false),
    };
    let display = path.display().to_string();
    let text = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((
                Mapping::default(),
                format!("none ({display} does not exist)"),
            ));
        }
        Err(source) => {
            return Err(ShareError::MappingRead {
                path: display,
                source,
            });
        }
    };
    let rules = crate::sanitize::mapping::parse(&text).map_err(|source| ShareError::Mapping {
        path: display.clone(),
        source,
    })?;
    let count = rules.rules().len();
    let noun = if count == 1 { "rule" } else { "rules" };
    Ok((rules, format!("{display} ({count} {noun})")))
}

#[must_use]
pub fn rule_counts(report: &Report) -> String {
    let mut by_rule: Vec<(String, usize)> = Vec::new();
    for change in &report.changes {
        let name = change.rule.to_string();
        match by_rule.iter_mut().find(|(rule, _)| *rule == name) {
            Some((_, count)) => *count += 1,
            None => by_rule.push((name, 1)),
        }
    }
    by_rule.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    if by_rule.is_empty() {
        return "no rule matched".to_owned();
    }
    by_rule
        .iter()
        .map(|(rule, count)| format!("{rule} {count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub const NOT_SCANNED: &str =
    "not scanned: dates, roles, finalize; numbers, booleans and null stay typed";
pub const BEST_EFFORT: &str = "the sanitizer is best-effort: it catches known vendor tokens, assignments, name/value pairs, URL credentials, PEM blocks and mapped terms, and can miss unusual code forms, new vendor shapes and bare-word passwords in prose; read the report before you share the bundle";

#[must_use]
pub fn summary(report: &Report) -> String {
    format!(
        "review summary: changes by rule: {}\n{NOT_SCANNED}\n{BEST_EFFORT}\n",
        rule_counts(report)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_and_the_local_review_sit_beside_the_bundle() {
        assert_eq!(
            BundlePaths::beside(Path::new("out/s.clean.json")),
            BundlePaths {
                bundle: PathBuf::from("out/s.clean.json"),
                report: PathBuf::from("out/s.clean.report.md"),
                local: PathBuf::from("out/s.clean.review.local.json"),
            }
        );
        assert_eq!(
            BundlePaths::beside(Path::new("bundle")).report,
            PathBuf::from("bundle.report.md")
        );
    }

    #[tokio::test]
    async fn refuse_keeps_an_existing_file_and_replace_overwrites_it() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("s.report.md");
        std::fs::write(&path, "marks typed by hand").expect("seed");
        let refused = write_file(path.clone(), b"new".to_vec(), Clobber::Refuse).await;
        assert_eq!(
            refused.map_err(|error| error.kind()),
            Err(std::io::ErrorKind::AlreadyExists)
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("kept"),
            "marks typed by hand"
        );
        write_file(path.clone(), b"new".to_vec(), Clobber::Replace)
            .await
            .expect("replaced");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "new");
        let left: Vec<_> = std::fs::read_dir(directory.path())
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(left, [std::ffi::OsString::from("s.report.md")]);
    }
}
