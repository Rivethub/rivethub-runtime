use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::path::PathBuf;

use super::HookError;
use crate::listing::ClaudeFile;
use crate::listing::session_agents;
use crate::payload::HookEvent;
use crate::payload::HookPayload;
use crate::store_path::single_component;

const DIR: &str = "claude-subagents";
const DIR_MODE: u32 = 0o700;

#[derive(Debug)]
pub(super) struct Selection {
    pub(super) files: Vec<ClaudeFile>,
    pub(super) named: bool,
}

pub(super) async fn select(
    payload: &HookPayload,
    native: &str,
    event: HookEvent,
) -> Result<Selection, HookError> {
    let named = match event {
        HookEvent::SubagentStop => named(payload, native),
        HookEvent::Stop | HookEvent::SessionEnd => None,
        HookEvent::PostToolUse | HookEvent::UserPromptSubmit | HookEvent::Other => {
            return Ok(Selection {
                files: Vec::new(),
                named: false,
            });
        }
    };
    if let Some(file) = named
        && tokio::fs::try_exists(&file.path).await?
    {
        return Ok(Selection {
            files: vec![file],
            named: true,
        });
    }
    let files = match payload.transcript_path.as_deref().and_then(Path::parent) {
        Some(slug) => session_agents(slug, native)?,
        None => Vec::new(),
    };
    Ok(Selection {
        files,
        named: false,
    })
}

fn named(payload: &HookPayload, native: &str) -> Option<ClaudeFile> {
    let listed = payload.agent_transcript_path.as_deref();
    let id = listed
        .and_then(agent_id)
        .or(payload.agent_id.as_deref())
        .filter(|id| single_component(id))?;
    let path = match listed {
        Some(path) => path.to_path_buf(),
        None => payload
            .transcript_path
            .as_deref()?
            .parent()?
            .join(native)
            .join("subagents")
            .join(format!("agent-{id}.jsonl")),
    };
    Some(ClaudeFile {
        id: id.to_owned(),
        parent: Some(native.to_owned()),
        path,
    })
}

fn agent_id(path: &Path) -> Option<&str> {
    path.file_name()?
        .to_str()?
        .strip_prefix("agent-")?
        .strip_suffix(".jsonl")
}

#[derive(Debug)]
pub(super) struct Captured {
    dir: PathBuf,
}

impl Captured {
    pub(super) fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join(DIR),
        }
    }

    fn file(&self, native: &str, id: &str) -> Option<PathBuf> {
        (single_component(native) && single_component(id)).then(|| self.dir.join(native).join(id))
    }

    pub(super) async fn holds(&self, native: &str, id: &str, bytes: u64) -> std::io::Result<bool> {
        let Some(file) = self.file(native, id) else {
            return Ok(false);
        };
        if !tokio::fs::try_exists(&file).await? {
            return Ok(false);
        }
        Ok(tokio::fs::read(&file).await? == bytes.to_string().as_bytes())
    }

    pub(super) async fn record(&self, native: &str, id: &str, bytes: u64) -> std::io::Result<()> {
        let Some(file) = self.file(native, id) else {
            return Ok(());
        };
        if let Some(dir) = file.parent() {
            tokio::fs::create_dir_all(dir).await?;
            tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE)).await?;
        }
        tokio::fs::write(file, bytes.to_string()).await
    }
}
