mod ancestors;
mod claude;
pub(crate) mod codex;
pub(crate) mod cowork;
mod cursor;
mod databases;
mod field;
mod files;
mod grok;
mod history;
mod kimi;
mod omp;
mod roots;
mod running;
mod text;
pub(crate) mod time;
mod title;
mod transcripts;

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use kernel::transcript::ListLimit;
use kernel::transcript::SessionLister;
use kernel::transcript::SessionRenameError;
use kernel::transcript::StoreSession;
use kernel::transcript::ToolOutputRead;
use kernel::transcript::TranscriptRead;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::TranscriptWindow;
use wire::harness::HarnessId;

pub use ancestors::with_ancestors;
pub use roots::claude_config_file;
pub use roots::store_roots;
pub use transcripts::StoreTranscriptReader;

use crate::readers::blocking;

pub const LISTING_ORDER: [HarnessId; 9] = [
    HarnessId::ClaudeCode,
    HarnessId::GrokBuild,
    HarnessId::Hermes,
    HarnessId::KimiCode,
    HarnessId::Codex,
    HarnessId::Opencode,
    HarnessId::Omp,
    HarnessId::Cursor,
    HarnessId::Cowork,
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreRoots {
    pub claude: Option<PathBuf>,
    pub grok: Option<PathBuf>,
    pub hermes: Option<PathBuf>,
    pub kimi: Option<PathBuf>,
    pub codex: Option<PathBuf>,
    pub opencode: Option<PathBuf>,
    pub omp: Option<PathBuf>,
    pub cursor: Option<PathBuf>,
    pub cowork: Vec<PathBuf>,
    pub dsh: Option<PathBuf>,
}

impl StoreRoots {
    fn single(&self, harness: HarnessId) -> Option<&Path> {
        match harness {
            HarnessId::ClaudeCode => self.claude.as_deref(),
            HarnessId::GrokBuild => self.grok.as_deref(),
            HarnessId::Hermes => self.hermes.as_deref(),
            HarnessId::KimiCode => self.kimi.as_deref(),
            HarnessId::Codex => self.codex.as_deref(),
            HarnessId::Opencode => self.opencode.as_deref(),
            HarnessId::Omp => self.omp.as_deref(),
            HarnessId::Cursor => self.cursor.as_deref(),
            HarnessId::Dsh | HarnessId::Cowork => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StoreSessionLister {
    roots: Arc<StoreRoots>,
    spawns: Arc<grok::SpawnIndex>,
}

impl StoreSessionLister {
    #[must_use]
    pub fn new(roots: StoreRoots) -> Self {
        Self {
            roots: Arc::new(roots),
            spawns: Arc::new(grok::SpawnIndex::default()),
        }
    }

    pub async fn harness(
        &self,
        harness: HarnessId,
        limit: usize,
    ) -> Result<Vec<StoreSession>, TranscriptReaderError> {
        let roots = Arc::clone(&self.roots);
        let spawns = Arc::clone(&self.spawns);
        blocking(move || list_harness(&roots, &spawns, harness, limit)).await
    }
}

pub(crate) async fn grok_title(
    root: PathBuf,
    id: String,
) -> Result<Option<String>, TranscriptReaderError> {
    blocking(move || grok::title(&root, &id)).await
}

pub(crate) async fn opencode_title(
    root: PathBuf,
    id: String,
) -> Result<Option<String>, TranscriptReaderError> {
    blocking(move || {
        Ok(databases::opencode_describe(&root, &id)?
            .map(|row| row.title)
            .filter(|title| *title != id))
    })
    .await
}

pub(crate) async fn codex_session_title(
    root: PathBuf,
    id: String,
) -> Result<Option<String>, TranscriptReaderError> {
    blocking(move || {
        Ok(codex::describe(&root, &id)?
            .map(|row| row.title)
            .filter(|title| *title != id))
    })
    .await
}

fn list_harness(
    roots: &StoreRoots,
    spawns: &grok::SpawnIndex,
    harness: HarnessId,
    limit: usize,
) -> Result<Vec<StoreSession>, TranscriptReaderError> {
    match harness {
        HarnessId::ClaudeCode => {
            absent_store(roots.claude.as_deref(), |root| claude::list(root, limit))
        }
        HarnessId::GrokBuild => absent_store(roots.grok.as_deref(), |root| {
            grok::list(root, spawns, limit)
        }),
        HarnessId::Hermes => absent_store(roots.hermes.as_deref(), |root| {
            databases::hermes_list(root, limit)
        }),
        HarnessId::KimiCode => absent_store(roots.kimi.as_deref(), |root| kimi::list(root, limit)),
        HarnessId::Codex => absent_store(roots.codex.as_deref(), |root| codex::list(root, limit)),
        HarnessId::Opencode => absent_store(roots.opencode.as_deref(), |root| {
            databases::opencode_list(root, limit)
        }),
        HarnessId::Omp => absent_store(roots.omp.as_deref(), |root| omp::list(root, limit)),
        HarnessId::Cursor => {
            absent_store(roots.cursor.as_deref(), |root| cursor::list(root, limit))
        }
        HarnessId::Cowork => Ok(cowork::list(&roots.cowork, limit)),
        HarnessId::Dsh => Ok(Vec::new()),
    }
}

fn describe_harness(
    roots: &StoreRoots,
    spawns: &grok::SpawnIndex,
    harness: HarnessId,
    native: &str,
    parent: Option<&str>,
) -> Result<Option<StoreSession>, TranscriptReaderError> {
    match harness {
        HarnessId::ClaudeCode => absent_store(roots.claude.as_deref(), |root| {
            claude::describe(root, native, parent)
        }),
        HarnessId::GrokBuild => absent_store(roots.grok.as_deref(), |root| {
            grok::describe(root, spawns, native)
        }),
        HarnessId::Hermes => absent_store(roots.hermes.as_deref(), |root| {
            databases::hermes_describe(root, native)
        }),
        HarnessId::KimiCode => {
            absent_store(roots.kimi.as_deref(), |root| kimi::describe(root, native))
        }
        HarnessId::Codex => {
            absent_store(roots.codex.as_deref(), |root| codex::describe(root, native))
        }
        HarnessId::Opencode => absent_store(roots.opencode.as_deref(), |root| {
            databases::opencode_describe(root, native)
        }),
        HarnessId::Omp => absent_store(roots.omp.as_deref(), |root| omp::describe(root, native)),
        HarnessId::Cursor => absent_store(roots.cursor.as_deref(), |root| {
            cursor::describe(root, native)
        }),
        HarnessId::Cowork => Ok(cowork::describe(&roots.cowork, native)),
        HarnessId::Dsh => Ok(None),
    }
}

fn describe_many_harness(
    roots: &StoreRoots,
    spawns: &grok::SpawnIndex,
    harness: HarnessId,
    natives: &[String],
) -> Result<Vec<StoreSession>, TranscriptReaderError> {
    match harness {
        HarnessId::GrokBuild => absent_store(roots.grok.as_deref(), |root| {
            grok::describe_many(root, spawns, natives)
        }),
        HarnessId::Codex => absent_store(roots.codex.as_deref(), |root| {
            codex::describe_many(root, natives)
        }),
        HarnessId::ClaudeCode
        | HarnessId::KimiCode
        | HarnessId::Hermes
        | HarnessId::Opencode
        | HarnessId::Omp
        | HarnessId::Dsh
        | HarnessId::Cursor
        | HarnessId::Cowork => natives
            .iter()
            .filter_map(|native| describe_harness(roots, spawns, harness, native, None).transpose())
            .collect(),
    }
}

fn store_cwd_of(
    roots: &StoreRoots,
    harness: HarnessId,
    native: &str,
) -> Result<Option<String>, TranscriptReaderError> {
    match harness {
        HarnessId::ClaudeCode => absent_store(roots.claude.as_deref(), |root| {
            claude::store_cwd(root, native)
        }),
        HarnessId::Cowork => Ok(cowork::describe(&roots.cowork, native).and_then(|row| row.cwd)),
        HarnessId::GrokBuild
        | HarnessId::Hermes
        | HarnessId::KimiCode
        | HarnessId::Codex
        | HarnessId::Opencode
        | HarnessId::Omp
        | HarnessId::Dsh
        | HarnessId::Cursor => Ok(None),
    }
}

fn permission_mode_of(
    roots: &StoreRoots,
    harness: HarnessId,
    native: &str,
) -> Result<Option<String>, TranscriptReaderError> {
    match harness {
        HarnessId::ClaudeCode => absent_store(roots.claude.as_deref(), |root| {
            claude::permission_mode(root, native)
        }),
        HarnessId::GrokBuild
        | HarnessId::Hermes
        | HarnessId::KimiCode
        | HarnessId::Codex
        | HarnessId::Opencode
        | HarnessId::Omp
        | HarnessId::Dsh
        | HarnessId::Cursor
        | HarnessId::Cowork => Ok(None),
    }
}

fn rename_harness(
    roots: &StoreRoots,
    spawns: &grok::SpawnIndex,
    harness: HarnessId,
    native: &str,
    title: &str,
) -> Result<StoreSession, SessionRenameError> {
    let renamed = match harness {
        HarnessId::ClaudeCode => match roots.claude.as_deref() {
            Some(root) => claude::rename(root, native, title)?,
            None => None,
        },
        HarnessId::GrokBuild => match roots.grok.as_deref() {
            Some(root) => grok::rename(root, spawns, native, title)?,
            None => None,
        },
        HarnessId::Hermes
        | HarnessId::KimiCode
        | HarnessId::Codex
        | HarnessId::Opencode
        | HarnessId::Omp
        | HarnessId::Dsh
        | HarnessId::Cursor
        | HarnessId::Cowork => return Err(SessionRenameError::Unsupported(harness)),
    };
    renamed.ok_or(SessionRenameError::Missing)
}

fn absent_store<T: Default>(
    root: Option<&Path>,
    work: impl FnOnce(&Path) -> Result<T, TranscriptReaderError>,
) -> Result<T, TranscriptReaderError> {
    match root {
        Some(root) => work(root),
        None => Ok(T::default()),
    }
}

pub fn rank(mut rows: Vec<StoreSession>, limit: usize) -> Vec<StoreSession> {
    rows.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
    with_ancestors(rows, limit)
}

#[async_trait]
impl SessionLister for StoreSessionLister {
    async fn list(&self, harnesses: &[HarnessId], limit: ListLimit) -> Vec<StoreSession> {
        let mut rows = Vec::new();
        for harness in LISTING_ORDER {
            if !harnesses.contains(&harness) {
                continue;
            }
            match self.harness(harness, limit.get()).await {
                Ok(found) => rows.extend(found),
                Err(error) => {
                    tracing::debug!(%error, %harness, "harness session store failed and lists as empty");
                }
            }
        }
        rank(rows, limit.get())
    }

    async fn describe(
        &self,
        harness: HarnessId,
        native: &str,
        parent: Option<&str>,
    ) -> Result<Option<StoreSession>, TranscriptReaderError> {
        let roots = Arc::clone(&self.roots);
        let spawns = Arc::clone(&self.spawns);
        let native = native.to_owned();
        let parent = parent.map(str::to_owned);
        blocking(move || describe_harness(&roots, &spawns, harness, &native, parent.as_deref()))
            .await
    }

    async fn describe_many(
        &self,
        harness: HarnessId,
        natives: &[String],
    ) -> Result<Vec<StoreSession>, TranscriptReaderError> {
        let roots = Arc::clone(&self.roots);
        let spawns = Arc::clone(&self.spawns);
        let natives = natives.to_vec();
        blocking(move || describe_many_harness(&roots, &spawns, harness, &natives)).await
    }

    async fn store_cwd(
        &self,
        harness: HarnessId,
        native: &str,
    ) -> Result<Option<String>, TranscriptReaderError> {
        let roots = Arc::clone(&self.roots);
        let native = native.to_owned();
        blocking(move || store_cwd_of(&roots, harness, &native)).await
    }

    async fn permission_mode(
        &self,
        harness: HarnessId,
        native: &str,
    ) -> Result<Option<String>, TranscriptReaderError> {
        let roots = Arc::clone(&self.roots);
        let native = native.to_owned();
        blocking(move || permission_mode_of(&roots, harness, &native)).await
    }

    async fn running_elsewhere(
        &self,
        harness: HarnessId,
        native: &str,
    ) -> Result<Option<u32>, TranscriptReaderError> {
        let Some(root) = self
            .roots
            .single(harness)
            .filter(|_| harness == HarnessId::ClaudeCode)
            .map(Path::to_path_buf)
        else {
            return Ok(None);
        };
        let native = native.to_owned();
        blocking(move || {
            running::claude_running(&root, &native)
                .map_err(|error| files::read_failure(&root.join("sessions"), error))
        })
        .await
    }

    async fn transcript(
        &self,
        harness: HarnessId,
        native: &str,
        window: TranscriptWindow,
    ) -> Result<TranscriptRead, TranscriptReaderError> {
        history::transcript(&self.roots, harness, native, window).await
    }

    async fn rename(
        &self,
        harness: HarnessId,
        native: &str,
        title: &str,
    ) -> Result<StoreSession, SessionRenameError> {
        let roots = Arc::clone(&self.roots);
        let spawns = Arc::clone(&self.spawns);
        let native = native.to_owned();
        let title = title.to_owned();
        blocking(move || Ok(rename_harness(&roots, &spawns, harness, &native, &title))).await?
    }

    async fn tool_output(
        &self,
        harness: HarnessId,
        native: &str,
        tool_call_id: &str,
    ) -> Result<ToolOutputRead, TranscriptReaderError> {
        history::tool_output(&self.roots, harness, native, tool_call_id).await
    }
}
