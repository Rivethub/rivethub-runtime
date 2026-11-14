use std::path::Path;

use capture::share_policy::SharePolicy;
use chrono::{DateTime, Utc};
use config::{Config, Environment, Paths};
use nix::sys::statvfs::{Statvfs, statvfs};
use serde::{Deserialize, Serialize};
use sqlx::Row as _;

use crate::{Error, Exit, Report, configuration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::Display)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum CheckName {
    Config,
    StateDirectory,
    DiskSpace,
    CaptureSpool,
    Memory,
    Policy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: CheckName,
    pub status: CheckStatus,
    pub message: String,
}

impl Check {
    fn pass(name: CheckName, message: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Pass,
            message: message.into(),
        }
    }

    fn fail(name: CheckName, error: impl std::fmt::Display) -> Self {
        Self {
            name,
            status: CheckStatus::Fail,
            message: error.to_string(),
        }
    }

    fn warn(name: CheckName, message: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Warn,
            message: message.into(),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub version: String,
    pub timestamp: DateTime<Utc>,
    pub checks: Vec<Check>,
    pub summary: Summary,
}

impl DoctorReport {
    fn new(checks: Vec<Check>) -> Self {
        let mut summary = Summary::default();
        for check in &checks {
            match check.status {
                CheckStatus::Pass => summary.pass += 1,
                CheckStatus::Warn => summary.warn += 1,
                CheckStatus::Fail => summary.fail += 1,
            }
        }
        Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            timestamp: Utc::now(),
            checks,
            summary,
        }
    }

    fn render(&self, json: bool) -> Result<Report, Error> {
        let stdout = if json {
            format!("{}\n", serde_json::to_string(self)?)
        } else {
            let lines: Vec<_> = self
                .checks
                .iter()
                .map(|check| format!("{}: {} — {}", check.status, check.name, check.message))
                .collect();
            format!("{}\n", lines.join("\n"))
        };
        Ok(Report {
            stdout,
            stderr: String::new(),
            exit: if self.summary.fail == 0 {
                Exit::Ok
            } else {
                Exit::Failure
            },
            stdout_raw: Vec::new(),
        })
    }
}

pub(crate) async fn run(env: &Environment, json: bool) -> Result<Report, Error> {
    let state = Paths::new(env)?.state_dir();
    let loaded = configuration::load(env);
    let directories = tokio::task::spawn_blocking(move || state_checks(&state));
    let (loaded, directories) = tokio::join!(loaded, directories);
    let mut checks = Vec::new();
    let mut stderr = String::new();
    match loaded {
        Ok((_, loaded)) => {
            stderr = configuration::notices(&loaded);
            let validation = config::ValidationResult {
                errors: Vec::new(),
                warnings: loaded.warnings,
            };
            checks.push(Check {
                name: CheckName::Config,
                status: if validation.warnings.is_empty() {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Warn
                },
                message: validation.to_string(),
            });
            checks.extend(directories?);
            checks.push(spool_check(env).await);
            checks.push(schema_check(&loaded.config, env).await);
            checks.push(policy_check(&loaded.config));
        }
        Err(error) => {
            checks.push(Check::fail(CheckName::Config, error));
            checks.extend(directories?);
            checks.push(Check::fail(
                CheckName::Memory,
                "Cannot verify memory without valid configuration.",
            ));
        }
    }
    let mut report = DoctorReport::new(checks).render(json)?;
    report.stderr = stderr;
    Ok(report)
}

async fn spool_check(env: &Environment) -> Check {
    match crate::capture_hook::spool_warning(env).await {
        Ok(Some(warning)) => Check::warn(CheckName::CaptureSpool, warning),
        Ok(None) => Check::pass(
            CheckName::CaptureSpool,
            "The capture spool is within bounds.",
        ),
        Err(error) => Check::fail(CheckName::CaptureSpool, error),
    }
}

fn policy_check(config: &Config) -> Check {
    let state = |off: bool| if off { "off" } else { "on" };
    match SharePolicy::from_config(config) {
        Ok(sharing) => Check::pass(
            CheckName::Policy,
            format!(
                "cloud {}, telemetry {}, sharing with {sharing}",
                state(config.cloud_disabled()),
                state(config.telemetry_disabled())
            ),
        ),
        Err(error) => Check::fail(CheckName::Policy, error),
    }
}

fn state_checks(path: &Path) -> Vec<Check> {
    let writable =
        match tempfile::NamedTempFile::new_in(path).and_then(tempfile::NamedTempFile::close) {
            Ok(()) => Check::pass(
                CheckName::StateDirectory,
                "State directory exists and is writable.",
            ),
            Err(error) => Check::fail(CheckName::StateDirectory, error),
        };
    let disk = match statvfs(path) {
        Ok(stats) => disk_check(available_bytes(&stats)),
        Err(error) => Check::fail(CheckName::DiskSpace, error),
    };
    vec![writable, disk]
}

#[cfg_attr(
    target_os = "linux",
    expect(
        clippy::useless_conversion,
        reason = "fsblkcnt_t is u64 on Linux but u32 on macOS"
    )
)]
fn available_bytes(stats: &Statvfs) -> u64 {
    u64::from(stats.blocks_available()).saturating_mul(stats.fragment_size())
}

fn disk_check(bytes: u64) -> Check {
    let status = if bytes < 256 * 1024 * 1024 {
        CheckStatus::Fail
    } else if bytes < 1024 * 1024 * 1024 {
        CheckStatus::Warn
    } else {
        CheckStatus::Pass
    };
    Check {
        name: CheckName::DiskSpace,
        status,
        message: format!("{} MB available.", bytes / (1024 * 1024)),
    }
}

async fn schema_check(config: &Config, env: &Environment) -> Check {
    let foreign = match foreign_note(config).await {
        Ok(note) => note,
        Err(error) => return Check::fail(CheckName::Memory, error),
    };
    match schema_status(config, env).await {
        Ok(status) if status.current == status.required && status.pending == 0 => {
            match duplicate_note(config, env).await {
                Ok(parked) => current_memory(status.current, foreign.as_deref(), parked.as_deref()),
                Err(error) => Check::warn(
                    CheckName::Memory,
                    format!(
                        "Schema {} is current. Duplicate checks could not be counted: {error}",
                        status.current
                    ),
                ),
            }
        }
        Ok(status) => {
            let extra = match &foreign {
                Some(note) => format!(" {note}"),
                None => String::new(),
            };
            Check::fail(
                CheckName::Memory,
                format!(
                    "Schema {} is not current; required {}, pending {}. Run rivethub-migrate explicitly.{extra}",
                    status.current, status.required, status.pending
                ),
            )
        }
        Err(error) => Check::fail(CheckName::Memory, error),
    }
}

async fn foreign_note(config: &Config) -> Result<Option<String>, Error> {
    if !config.sqlite_selected() {
        return Ok(None);
    }
    let Some(path) = config
        .memory
        .as_ref()
        .and_then(|memory| memory.sqlite.as_ref())
        .and_then(|sqlite| sqlite.path.clone())
    else {
        return Ok(None);
    };
    let objects = tokio::task::spawn_blocking(move || migrate::sqlite_foreign(&path)).await??;
    if objects.is_empty() {
        Ok(None)
    } else {
        let body = objects
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        Ok(Some(format!("Foreign schema: {body}.")))
    }
}

async fn schema_status(config: &Config, env: &Environment) -> Result<migrate::Status, Error> {
    if config.sqlite_selected() {
        let path = config
            .memory
            .as_ref()
            .and_then(|memory| memory.sqlite.as_ref())
            .and_then(|sqlite| sqlite.path.clone())
            .ok_or(Error::SqlitePath)?;
        Ok(tokio::task::spawn_blocking(move || migrate::sqlite_status(&path)).await??)
    } else {
        let url = config.memory_postgres_url(env).ok_or(Error::PostgresUrl)?;
        Ok(migrate::postgres_status(&url).await?)
    }
}

fn current_memory(current: i64, foreign: Option<&str>, parked: Option<&str>) -> Check {
    let mut message = format!("Schema {current} is current.");
    if let Some(foreign) = foreign {
        message.push(' ');
        message.push_str(foreign);
    }
    if let Some(parked) = parked {
        message.push(' ');
        message.push_str(parked);
    }
    if foreign.is_some() || parked.is_some() {
        Check::warn(CheckName::Memory, message)
    } else {
        Check::pass(CheckName::Memory, message)
    }
}

async fn duplicate_note(config: &Config, env: &Environment) -> Result<Option<String>, sqlx::Error> {
    if config.sqlite_selected() {
        return Ok(None);
    }
    let Some(url) = config.memory_postgres_url(env) else {
        return Ok(None);
    };
    let (failed, held) = parked_counts(&url).await?;
    Ok(parked_sentence(failed, held))
}

async fn parked_counts(url: &str) -> Result<(i64, i64), sqlx::Error> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(url)
        .await?;
    sqlx::query("SELECT set_config('rh.maintenance', 'on', false)")
        .execute(&pool)
        .await?;
    let row = sqlx::query(
        "SELECT
            (SELECT count(*) FROM rh_entity_extractions
              WHERE status = 'failed' AND error ILIKE '%match undecided%') AS failed,
            (SELECT count(*) FROM rh_entity_candidates) AS held",
    )
    .fetch_one(&pool)
    .await?;
    pool.close().await;
    let failed: i64 = row.try_get("failed")?;
    let held: i64 = row.try_get("held")?;
    Ok((failed, held))
}

fn parked_sentence(failed: i64, held: i64) -> Option<String> {
    let mut parts = Vec::new();
    if failed > 0 {
        parts.push(format!(
            "{} failed because the duplicate check could not run",
            count_phrase(failed, "entity extraction", "entity extractions")
        ));
    }
    if held > 0 {
        parts.push(format!(
            "{} parked",
            count_phrase(held, "entity match is", "entity matches are")
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("{}.", parts.join(" and ")))
    }
}

fn count_phrase(count: i64, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckStatus, disk_check};

    #[test]
    fn disk_thresholds_are_inclusive_and_fail_closed() {
        for (bytes, expected) in [
            (0, CheckStatus::Fail),
            (256 * 1024 * 1024 - 1, CheckStatus::Fail),
            (256 * 1024 * 1024, CheckStatus::Warn),
            (1024 * 1024 * 1024 - 1, CheckStatus::Warn),
            (1024 * 1024 * 1024, CheckStatus::Pass),
        ] {
            assert_eq!(disk_check(bytes).status, expected);
        }
    }

    #[test]
    fn parked_duplicate_checks_are_a_warning() {
        assert_eq!(super::parked_sentence(0, 0), None);
        assert_eq!(
            super::parked_sentence(1, 0).as_deref(),
            Some("1 entity extraction failed because the duplicate check could not run.")
        );
        assert_eq!(
            super::parked_sentence(2, 1).as_deref(),
            Some(
                "2 entity extractions failed because the duplicate check could not run and 1 entity match is parked."
            )
        );
        let quiet = super::current_memory(71, None, None);
        assert_eq!(quiet.status, CheckStatus::Pass);
        assert_eq!(quiet.message, "Schema 71 is current.");
        let parked = super::current_memory(71, None, Some("1 entity match is parked."));
        assert_eq!(parked.status, CheckStatus::Warn);
    }
}
