use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use config::Environment;
use kernel::harness_setup::CaptureMethod;
use kernel::harness_setup::ConnectOutcome;
use kernel::harness_setup::ConnectPart;
use kernel::harness_setup::HarnessProbe;
use kernel::harness_setup::HarnessSetup;
use kernel::harness_setup::RecallState;
use kernel::harness_setup::RecallStatus;
use kernel::harness_setup::SetupError;

use super::codex_trust;
use super::codex_trust::Trust;
use super::dsh;
use super::install;
use super::locate::Locator;
use super::opencode_version::Fit;
use super::plan;
use super::plan::Backups;
use super::plan::FileEdit;
use super::recall;
use super::registry::CaptureInstall;
use super::registry::HarnessSpec;
use super::registry::REGISTRY;
use super::registry::spec;
use super::version;
use super::version::Version;

const NOTHING_TO_INSTALL: &str =
    "rh reads this harness's own store, or the client pushes, so there is nothing to install";
const NOT_FOUND: &str = "it is not installed on this machine";

#[derive(Debug)]
pub struct SetupChange {
    pub outcome: ConnectOutcome,
    pub skipped: Option<SetupError>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct HarnessSetupService {
    locator: Arc<Locator>,
    rh: PathBuf,
    backups: Backups,
}

impl HarnessSetupService {
    #[must_use]
    pub fn new(env: &Environment, rh: PathBuf, backups: PathBuf) -> Self {
        Self {
            locator: Arc::new(Locator::new(env)),
            rh,
            backups: Backups::new(backups),
        }
    }

    #[must_use]
    pub fn probe_now(&self) -> Vec<HarnessProbe> {
        REGISTRY
            .iter()
            .map(|spec| probe_one(&self.locator, spec, &self.rh).0)
            .collect()
    }

    pub fn connect_now(
        &self,
        id: &str,
        part: Option<ConnectPart>,
        dry_run: bool,
        now: DateTime<Utc>,
    ) -> Result<SetupChange, SetupError> {
        let spec = known(id)?;
        let (probe, version) = probe_one(&self.locator, spec, &self.rh);
        if !probe.found {
            return Err(SetupError::Unsupported {
                id: spec.id.to_owned(),
                part: part.unwrap_or(ConnectPart::Capture),
                reason: NOT_FOUND,
            });
        }
        let (edits, skipped) = self.plan(spec, part, Direction::Connect, version)?;
        let outcome = plan::apply(&edits, &self.backups, id, dry_run, now)?;
        let capture_planned = part != Some(ConnectPart::Recall)
            && !matches!(
                skipped,
                Some(SetupError::Unsupported {
                    part: ConnectPart::Capture,
                    ..
                })
            );
        let notes = match &spec.install {
            Some(CaptureInstall::OpencodePlugin { .. }) if capture_planned => {
                vec![install::connect_note(Fit::of(version))]
            }
            Some(CaptureInstall::HookFile(hooks)) if capture_planned && spec.id == "codex" => {
                let trust = if dry_run && !edits.is_empty() {
                    Trust::Unknown
                } else {
                    codex_trust::read(&self.locator, hooks, version)
                };
                codex_trust::connect_note(trust).into_iter().collect()
            }
            _ => Vec::new(),
        };
        Ok(SetupChange {
            outcome,
            skipped,
            notes,
        })
    }

    pub fn disconnect_now(
        &self,
        id: &str,
        part: Option<ConnectPart>,
        dry_run: bool,
        now: DateTime<Utc>,
    ) -> Result<SetupChange, SetupError> {
        let spec = known(id)?;
        let (edits, skipped) = self.plan(spec, part, Direction::Disconnect, None)?;
        let outcome = plan::apply(&edits, &self.backups, id, dry_run, now)?;
        Ok(SetupChange {
            outcome,
            skipped,
            notes: Vec::new(),
        })
    }

    fn plan(
        &self,
        spec: &'static HarnessSpec,
        part: Option<ConnectPart>,
        direction: Direction,
        version: Option<Version>,
    ) -> Result<(Vec<FileEdit>, Option<SetupError>), SetupError> {
        let explicit = part.is_some();
        let parts = part.map_or_else(
            || vec![ConnectPart::Capture, ConnectPart::Recall],
            |part| vec![part],
        );
        if spec.id == "dsh" {
            return dsh::plan(
                &self.locator,
                &self.rh,
                &parts,
                direction == Direction::Connect,
            );
        }
        let mut edits = Vec::new();
        let mut planned = false;
        let mut refusal = None;
        for part in parts {
            match self.plan_part(spec, part, direction, version) {
                Ok(more) => {
                    planned = true;
                    edits.extend(more);
                }
                Err(error @ SetupError::Unsupported { .. }) if !explicit => refusal = Some(error),
                Err(error) => return Err(error),
            }
        }
        match (planned, refusal) {
            (false, Some(error)) => Err(error),
            (true, skipped) => Ok((edits, skipped)),
            (false, None) => Ok((edits, None)),
        }
    }

    fn plan_part(
        &self,
        spec: &'static HarnessSpec,
        part: ConnectPart,
        direction: Direction,
        version: Option<Version>,
    ) -> Result<Vec<FileEdit>, SetupError> {
        match (part, direction) {
            (ConnectPart::Capture, Direction::Connect) => match &spec.install {
                Some(target) => {
                    install::plan_connect(&self.locator, spec.id, target, &self.rh, version)
                }
                None => Err(nothing_to_install(spec, part)),
            },
            (ConnectPart::Capture, Direction::Disconnect) => match &spec.install {
                Some(target) => install::plan_disconnect(&self.locator, spec.id, target),
                None => Err(nothing_to_install(spec, part)),
            },
            (ConnectPart::Recall, Direction::Connect) => {
                recall::plan_connect(&self.locator, spec.id, &spec.recall, &self.rh)
            }
            (ConnectPart::Recall, Direction::Disconnect) => {
                recall::plan_disconnect(&self.locator, spec.id, &spec.recall)
            }
        }
    }
}

fn known(id: &str) -> Result<&'static HarnessSpec, SetupError> {
    spec(id).ok_or_else(|| SetupError::UnknownHarness(id.to_owned()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Connect,
    Disconnect,
}

fn nothing_to_install(spec: &HarnessSpec, part: ConnectPart) -> SetupError {
    SetupError::Unsupported {
        id: spec.id.to_owned(),
        part,
        reason: NOTHING_TO_INSTALL,
    }
}

fn probe_one(locator: &Locator, spec: &HarnessSpec, rh: &Path) -> (HarnessProbe, Option<Version>) {
    let binary = spec.binaries.iter().find_map(|name| locator.which(name));
    let version = match spec.id {
        "opencode" | "codex" => binary.as_deref().and_then(version::known),
        _ => None,
    };
    let data = spec.data.iter().find_map(|place| locator.existing(*place));
    let found = binary.is_some() || data.is_some();
    let path = binary
        .or_else(|| data.clone())
        .map(|path| path.display().to_string());
    let method = spec
        .capture
        .first()
        .copied()
        .unwrap_or(CaptureMethod::StoreScan);
    let capture = match &spec.install {
        Some(target) => install::detect(locator, target, spec.id, rh, version),
        None => match (method, &data) {
            (CaptureMethod::ClientPush, _) => install::InstallView {
                installed: false,
                connectable: false,
                detail: "the client pushes its sessions to POST /api/capture".to_owned(),
            },
            (_, Some(store)) => install::InstallView {
                installed: true,
                connectable: false,
                detail: format!("rh reads the store at {}", store.display()),
            },
            (_, None) => install::InstallView {
                installed: false,
                connectable: false,
                detail: "no store found to read".to_owned(),
            },
        },
    };
    let recall_view = recall::detect(locator, &spec.recall);
    let capture_open =
        spec.install.as_ref().is_some_and(CaptureInstall::writable) && capture.connectable;
    let recall_open = recall::writable(&spec.recall) && recall_view.state != RecallState::Connected;
    let probe = HarnessProbe {
        id: spec.id.to_owned(),
        name: spec.name.to_owned(),
        session_prefix: spec.session_prefix.to_owned(),
        found,
        path,
        version: version.map(|version| version.to_string()),
        method,
        capture_installed: capture.installed,
        install_detail: capture.detail,
        recall: RecallStatus {
            state: recall_view.state,
            detail: recall_view.detail,
        },
        can_connect: found && (capture_open || recall_open),
    };
    (probe, version)
}

#[async_trait]
impl HarnessSetup for HarnessSetupService {
    async fn probe(&self) -> Result<Vec<HarnessProbe>, SetupError> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || service.probe_now())
            .await
            .map_err(joined)
    }

    async fn connect(
        &self,
        id: &str,
        part: ConnectPart,
        dry_run: bool,
    ) -> Result<ConnectOutcome, SetupError> {
        let service = self.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            service
                .connect_now(&id, Some(part), dry_run, Utc::now())
                .map(|change| change.outcome)
        })
        .await
        .map_err(joined)?
    }
}

fn joined(error: tokio::task::JoinError) -> SetupError {
    SetupError::Io {
        path: "harness setup".to_owned(),
        source: std::io::Error::other(error),
    }
}
