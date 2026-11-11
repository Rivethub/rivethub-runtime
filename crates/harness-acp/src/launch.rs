use std::collections::BTreeMap;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use agent_client_protocol::schema::v1 as acp;
use config::Config;
use config::HarnessSlot;
use serde::Serialize;
use strum::IntoEnumIterator as _;
use wire::harness::HarnessId;

use crate::error::AcpError;
use crate::error::ChatUnsupportedReason;
use crate::error::UnverifiedReason;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpLaunch {
    Native(AcpCommand),
    Adapter(AcpCommand),
    Unverified {
        command: AcpCommand,
        reason: UnverifiedReason,
    },
}

impl AcpLaunch {
    #[must_use]
    pub fn command(&self) -> &AcpCommand {
        match self {
            Self::Native(command) | Self::Adapter(command) | Self::Unverified { command, .. } => {
                command
            }
        }
    }

    #[must_use]
    pub fn is_verified(&self) -> bool {
        !matches!(self, Self::Unverified { .. })
    }
}

#[derive(Debug, Clone)]
pub struct LaunchTable {
    entries: BTreeMap<HarnessId, LaunchEntry>,
}

#[derive(Debug, Clone)]
enum LaunchEntry {
    Launch(AcpLaunch),
    Unsupported(ChatUnsupportedReason),
}

impl LaunchTable {
    pub fn from_entries(entries: impl IntoIterator<Item = (HarnessId, AcpLaunch)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(harness, launch)| (harness, LaunchEntry::Launch(launch)))
                .collect(),
        }
    }

    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let mut entries = BTreeMap::new();
        for harness in HarnessId::iter() {
            entries.insert(harness, entry_for(config, harness));
        }
        Self { entries }
    }

    pub fn launch(&self, harness: HarnessId) -> Result<AcpLaunch, AcpError> {
        match self.entries.get(&harness) {
            Some(LaunchEntry::Launch(launch)) => Ok(launch.clone()),
            Some(LaunchEntry::Unsupported(reason)) => Err(AcpError::ChatUnsupported {
                harness,
                reason: *reason,
            }),
            None => Err(AcpError::Unconfigured { harness }),
        }
    }

    pub fn verified_command(&self, harness: HarnessId) -> Result<AcpCommand, AcpError> {
        match self.launch(harness)? {
            AcpLaunch::Native(command) | AcpLaunch::Adapter(command) => Ok(command),
            AcpLaunch::Unverified { command, reason } => Err(AcpError::UnverifiedLaunch {
                harness,
                program: command.program.display().to_string(),
                reason,
            }),
        }
    }

    #[must_use]
    pub fn facts(&self) -> Vec<LaunchFact> {
        self.entries
            .iter()
            .map(|(harness, entry)| LaunchFact::from_entry(*harness, entry))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchFact {
    pub harness: HarnessId,
    pub program: Option<PathBuf>,
    pub verified_chat: bool,
}

impl LaunchFact {
    fn from_entry(harness: HarnessId, entry: &LaunchEntry) -> Self {
        match entry {
            LaunchEntry::Launch(launch) => Self {
                harness,
                program: Some(launch.command().program.clone()),
                verified_chat: sheet_chat(harness),
            },
            LaunchEntry::Unsupported(_) => Self {
                harness,
                program: None,
                verified_chat: false,
            },
        }
    }
}

#[derive(Serialize)]
struct ClaudeCodeMeta {
    options: UnsavedOptions,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UnsavedOptions {
    persist_session: bool,
}

pub(crate) fn unsaved(
    harness: HarnessId,
    meta: Option<acp::Meta>,
) -> Result<Option<acp::Meta>, AcpError> {
    if harness != HarnessId::ClaudeCode {
        return Ok(meta);
    }
    let mut meta = match meta {
        Some(meta) => meta,
        None => acp::Meta::new(),
    };
    let claude_code = serde_json::to_value(ClaudeCodeMeta {
        options: UnsavedOptions {
            persist_session: false,
        },
    })
    .map_err(|source| AcpError::StartupMeta { source })?;
    meta.insert("claudeCode".to_owned(), claude_code);
    Ok(Some(meta))
}

fn sheet_chat(harness: HarnessId) -> bool {
    matches!(
        harness,
        HarnessId::ClaudeCode | HarnessId::Codex | HarnessId::Opencode
    )
}

fn entry_for(config: &Config, harness: HarnessId) -> LaunchEntry {
    match harness {
        HarnessId::Cursor => LaunchEntry::Unsupported(ChatUnsupportedReason::TerminalOnly),
        HarnessId::Cowork => LaunchEntry::Unsupported(ChatUnsupportedReason::CaptureOnly),
        HarnessId::GrokBuild => LaunchEntry::Launch(AcpLaunch::Native(acp_command(
            config,
            harness,
            "grok",
            vec!["agent".to_owned(), "stdio".to_owned()],
        ))),
        HarnessId::ClaudeCode => {
            let mut command = acp_command(config, harness, "claude-agent-acp", Vec::new());
            command
                .env
                .push(("CLAUDE_CODE_ENABLE_TODO_TOOLS".to_owned(), "1".to_owned()));
            LaunchEntry::Launch(AcpLaunch::Adapter(command))
        }
        HarnessId::Codex => LaunchEntry::Launch(AcpLaunch::Adapter(acp_command(
            config,
            harness,
            "codex-acp",
            Vec::new(),
        ))),
        HarnessId::Opencode => LaunchEntry::Launch(AcpLaunch::Native(acp_command(
            config,
            harness,
            "opencode",
            vec!["acp".to_owned()],
        ))),
        HarnessId::Hermes => LaunchEntry::Launch(AcpLaunch::Native(acp_command(
            config,
            harness,
            "hermes",
            vec!["acp".to_owned()],
        ))),
        HarnessId::KimiCode => LaunchEntry::Launch(AcpLaunch::Native(acp_command(
            config,
            harness,
            "kimi",
            vec!["acp".to_owned()],
        ))),
        HarnessId::Omp => LaunchEntry::Launch(AcpLaunch::Native(acp_command(
            config,
            harness,
            "omp",
            vec!["acp".to_owned()],
        ))),
        HarnessId::Dsh => LaunchEntry::Launch(AcpLaunch::Native(acp_command(
            config,
            harness,
            "dsh",
            vec!["acp".to_owned()],
        ))),
    }
}

fn acp_command(
    config: &Config,
    harness: HarnessId,
    program: &str,
    args: Vec<String>,
) -> AcpCommand {
    AcpCommand {
        program: configured(config, harness, program),
        args,
        working_dir: configured_dir(config, harness),
        env: Vec::new(),
    }
}

fn configured(config: &Config, harness: HarnessId, default: &str) -> PathBuf {
    config
        .tasks
        .as_ref()
        .and_then(|tasks| tasks.harnesses.as_ref())
        .and_then(|harnesses| harnesses.get(&HarnessSlot::Known(harness)))
        .and_then(|entry| entry.binary.as_ref())
        .map_or_else(|| PathBuf::from(default), PathBuf::from)
}

fn configured_dir(config: &Config, harness: HarnessId) -> Option<PathBuf> {
    config
        .tasks
        .as_ref()
        .and_then(|tasks| tasks.harnesses.as_ref())
        .and_then(|harnesses| harnesses.get(&HarnessSlot::Known(harness)))
        .and_then(|entry| entry.cwd.clone())
}

pub(crate) fn probe(
    facts: &[LaunchFact],
    search: &[PathBuf],
) -> Result<Vec<(LaunchFact, bool)>, std::io::Error> {
    let mut installed = Vec::with_capacity(facts.len());
    for fact in facts {
        let present = match &fact.program {
            Some(program) => program_installed(program, search)?,
            None => false,
        };
        installed.push((fact.clone(), present));
    }
    Ok(installed)
}

fn program_installed(program: &Path, search: &[PathBuf]) -> Result<bool, std::io::Error> {
    Ok(resolve_program(program, search)?.is_some())
}

pub(crate) fn resolve_program(
    program: &Path,
    search: &[PathBuf],
) -> Result<Option<PathBuf>, std::io::Error> {
    if program.as_os_str().is_empty()
        || program
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Ok(None);
    }
    if program.is_absolute() {
        return Ok(is_file(program)?.then(|| program.to_owned()));
    }
    for directory in search {
        let candidate = directory.join(program);
        if is_file(&candidate)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn is_file(path: &Path) -> Result<bool, std::io::Error> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
