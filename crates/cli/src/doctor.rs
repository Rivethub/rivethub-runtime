use std::path::Path;

use capture::share_policy::SharePolicy;
use chrono::{DateTime, Utc};
use config::{Config, Environment, Paths};
use nix::sys::statvfs::{Statvfs, statvfs};
use serde::{Deserialize, Serialize};

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
    Models,
    Service,
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
            if let Some(check) = model_check(&loaded.config) {
                checks.push(check);
            }
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
    if let Some(check) = service_check(env).await {
        checks.push(check);
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

async fn service_check(env: &Environment) -> Option<Check> {
    let env = env.clone();
    let checked = tokio::task::spawn_blocking(move || service_unit_check(&env)).await;
    let check = match checked {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    match check {
        Ok(check) => check,
        Err(message) => Some(Check::warn(CheckName::Service, message)),
    }
}

fn service_unit_check(env: &Environment) -> Result<Option<Check>, config::ConfigError> {
    let paths = Paths::new(env)?;
    let path =
        crate::service::Scope::for_uid(nix::unistd::geteuid().as_raw()).unit_path(paths.home());
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Ok(Some(Check::warn(
                CheckName::Service,
                format!(
                    "{path} could not be read ({error}), so its EnvironmentFile= was not checked.",
                    path = path.display()
                ),
            )));
        }
    };
    let ignored = crate::service::ignored_environment_files(&text);
    if ignored.is_empty() {
        return Ok(None);
    }
    Ok(Some(Check::warn(
        CheckName::Service,
        format!(
            "{path} has EnvironmentFile={values} which systemd ignores (the path is not absolute), so the environment file is not loaded. Run `rh service init` to update a unit rh generated.",
            path = path.display(),
            values = ignored.join(", ")
        ),
    )))
}

fn model_check(config: &Config) -> Option<Check> {
    if config.memory_models_available() {
        None
    } else {
        Some(Check::pass(CheckName::Models, config::NO_MODEL_NOTE))
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
            if let Some(foreign) = foreign {
                Check::warn(
                    CheckName::Memory,
                    format!("Schema {} is current. {foreign}", status.current),
                )
            } else {
                Check::pass(
                    CheckName::Memory,
                    format!("Schema {} is current.", status.current),
                )
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
}
