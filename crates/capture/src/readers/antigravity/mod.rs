mod capture;

use jiff::Timestamp;
use std::path::Path;
use std::path::PathBuf;

use async_trait::async_trait;
use kernel::transcript::TranscriptReader;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::Turn;
use rusqlite::Connection;
use rusqlite::OpenFlags;
use rusqlite::OptionalExtension as _;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use super::message::TranscriptSlice;
use super::message::blocking;
use super::message::native;
use super::message::project_turns;
use super::message::report;
use crate::records::SubagentLink;
use crate::store_path::Child;
use crate::store_path::DirState;
use crate::store_path::Root;
use crate::store_path::dir_state;
use crate::store_path::is_not_found;
use crate::store_path::session_component;
use crate::transcript::read_uncapped;

pub(crate) use capture::SpawnedSubagent;
pub(crate) use capture::spawned_subagents;

const FORMAT: &str = "antigravity-transcript";
const BRAIN_DIR: &str = "brain";
const FULL_LOG: &str = ".system_generated/logs/transcript_full.jsonl";
const COMPACT_LOG: &str = ".system_generated/logs/transcript.jsonl";
const SUMMARIES_DB: &str = "conversation_summaries.db";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SessionSummary {
    pub(crate) title: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) parent_conversation_id: Option<String>,
    pub(crate) agent_name: Option<String>,
    pub(crate) status: Option<String>,
}

#[derive(Debug)]
pub struct AntigravityTranscriptReader {
    roots: Vec<PathBuf>,
}

impl AntigravityTranscriptReader {
    #[must_use]
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    pub async fn messages(
        &self,
        session: &SessionId,
        offset: u64,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        let Some(native_id) = native(session, HarnessId::Antigravity).map(str::to_owned) else {
            return Ok(TranscriptSlice::empty(offset));
        };
        let roots = self.roots.clone();
        let slice = blocking(move || load(&roots, &native_id, offset)).await?;
        report(FORMAT, &slice.skipped);
        Ok(slice)
    }

    pub async fn capture(
        &self,
        session: &SessionId,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        self.messages(session, 0).await
    }
}

#[async_trait]
impl TranscriptReader for AntigravityTranscriptReader {
    async fn sessions(&self, store: &Path) -> Result<Vec<SessionId>, TranscriptReaderError> {
        let store = store.to_path_buf();
        blocking(move || list_sessions(&[store])).await
    }

    async fn read(&self, session: &SessionId) -> Result<Vec<Turn>, TranscriptReaderError> {
        Ok(project_turns(&self.messages(session, 0).await?))
    }

    async fn tail(
        &self,
        session: &SessionId,
        since: Timestamp,
    ) -> Result<Vec<Turn>, TranscriptReaderError> {
        Ok(self
            .read(session)
            .await?
            .into_iter()
            .filter(|turn| turn.at > since)
            .collect())
    }
}

pub(crate) fn capture_file(
    native_id: &str,
    path: &Path,
) -> Result<(PathBuf, String, TranscriptSlice), TranscriptReaderError> {
    let resolved = prefer_full_transcript(path);
    let text = read_uncapped(&resolved)?;
    let slice = capture::map_transcript(native_id, &resolved, &text, 0)?;
    report(FORMAT, &slice.skipped);
    Ok((resolved, text, slice))
}

#[must_use]
pub(crate) fn prefer_full_transcript(path: &Path) -> PathBuf {
    if path.file_name().and_then(|name| name.to_str()) == Some("transcript.jsonl") {
        let full = path.with_file_name("transcript_full.jsonl");
        if full.is_file() {
            return full;
        }
    }
    path.to_path_buf()
}

#[must_use]
pub(crate) fn app_dir_from_transcript(path: &Path) -> Option<PathBuf> {
    let logs = path.parent()?;
    let sys = logs.parent()?;
    let conv = sys.parent()?;
    let brain = conv.parent()?;
    (brain.file_name().and_then(|name| name.to_str()) == Some(BRAIN_DIR))
        .then(|| brain.parent().map(Path::to_path_buf))
        .flatten()
}

pub(crate) fn transcript_path_for(
    app_dir: &Path,
    native_id: &str,
) -> Result<Option<PathBuf>, TranscriptReaderError> {
    session_component(native_id)?;
    let root = match Root::open(app_dir) {
        Ok(root) => root,
        Err(error) if is_not_found(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    let folder = root.canonical().join(BRAIN_DIR).join(native_id);
    if dir_state(&root, &folder)? == DirState::Absent {
        return Ok(None);
    }
    for rel in [FULL_LOG, COMPACT_LOG] {
        let candidate = folder.join(rel);
        if root.classify(&candidate)? == Child::File {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

pub(crate) fn sessions(
    roots: &[PathBuf],
) -> Result<Vec<(String, PathBuf, PathBuf)>, TranscriptReaderError> {
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for app_dir in roots {
        let root = match Root::open(app_dir) {
            Ok(root) => root,
            Err(error) if is_not_found(&error) => continue,
            Err(error) => return Err(error),
        };
        let brain = root.canonical().join(BRAIN_DIR);
        if dir_state(&root, &brain)? == DirState::Absent {
            continue;
        }
        for folder in root.directories(&brain)? {
            let Some(id) = folder.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if session_component(id).is_err() || !seen.insert(id.to_owned()) {
                continue;
            }
            for rel in [FULL_LOG, COMPACT_LOG] {
                let candidate = folder.join(rel);
                if root.classify(&candidate)? == Child::File {
                    found.push((id.to_owned(), app_dir.clone(), candidate));
                    break;
                }
            }
        }
    }
    Ok(found)
}

pub(crate) fn summary_in(app_dir: &Path, native_id: &str) -> SessionSummary {
    let db = app_dir.join(SUMMARIES_DB);
    if !db.is_file() {
        return SessionSummary::default();
    }
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let Ok(conn) = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return SessionSummary::default();
    };
    let row = conn
        .query_row(
            "SELECT title, workspace_uris, parent_conversation_id, agent_name, status \
             FROM conversation_summaries WHERE conversation_id = ?1",
            [native_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional();
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let mut summary = match row {
        Ok(Some((title, workspaces, parent, agent, status))) => SessionSummary {
            title: nonempty(&title),
            cwd: first_workspace_path(&workspaces),
            parent_conversation_id: nonempty(&parent).filter(|p| p != native_id),
            agent_name: nonempty(&agent),
            status: nonempty(&status),
        },
        Ok(None) | Err(_) => SessionSummary::default(),
    };
    if summary.parent_conversation_id.is_none()
        && let Ok(Some(parent)) = conn
            .query_row(
                "SELECT parent_id FROM conversation_parents WHERE conversation_id = ?1",
                [native_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
    {
        summary.parent_conversation_id = nonempty(&parent).filter(|p| p != native_id);
    }
    summary
}

pub(crate) fn child_ids_in(app_dir: &Path, parent_id: &str) -> Vec<String> {
    let db = app_dir.join(SUMMARIES_DB);
    if !db.is_file() {
        return Vec::new();
    }
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let Ok(conn) = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for query in [
        "SELECT conversation_id FROM conversation_summaries WHERE parent_conversation_id = ?1",
        "SELECT conversation_id FROM conversation_parents WHERE parent_id = ?1",
    ] {
        #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
        let Ok(mut stmt) = conn.prepare(query) else {
            continue;
        };
        #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
        let Ok(rows) = stmt.query_map([parent_id], |row| row.get::<_, String>(0)) else {
            continue;
        };
        for id in rows.flatten() {
            if !id.is_empty() && id != parent_id && seen.insert(id.clone()) {
                ids.push(id);
            }
        }
    }
    ids
}

pub(crate) fn subagent_link_for(
    app_dir: &Path,
    native_id: &str,
    transcript_text: &str,
    summary: &SessionSummary,
    parent_spawn: Option<&SpawnedSubagent>,
) -> Option<SubagentLink> {
    let parent = summary
        .parent_conversation_id
        .clone()
        .or_else(|| capture::parent_from_transcript(transcript_text))
        .filter(|id| !id.is_empty() && id != native_id)?;

    let spawn_from_parent = if parent_spawn.is_none() {
        find_spawn_in_parent(app_dir, &parent, native_id)
    } else {
        None
    };
    let spawn = parent_spawn.or(spawn_from_parent.as_ref());

    Some(SubagentLink {
        parent_session_id: parent,
        agent_id: native_id.to_owned(),
        agent_type: summary
            .agent_name
            .clone()
            .or_else(|| spawn.and_then(|s| s.agent_type.clone())),
        description: spawn
            .and_then(|s| s.description.clone())
            .or_else(|| summary.title.clone()),
        tool_use_id: spawn.map(|s| s.tool_use_id.clone()),
    })
}

fn find_spawn_in_parent(
    app_dir: &Path,
    parent_id: &str,
    child_id: &str,
) -> Option<SpawnedSubagent> {
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let path = transcript_path_for(app_dir, parent_id).ok().flatten()?;
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let text = read_uncapped(&path).ok()?;
    spawned_subagents(parent_id, &text)
        .into_iter()
        .find(|spawn| spawn.conversation_id == child_id)
}

fn list_sessions(roots: &[PathBuf]) -> Result<Vec<SessionId>, TranscriptReaderError> {
    let mut ids: Vec<SessionId> = sessions(roots)?
        .into_iter()
        .map(|(id, _, _)| SessionId::new(HarnessId::Antigravity, &id))
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn load(
    roots: &[PathBuf],
    native_id: &str,
    offset: u64,
) -> Result<TranscriptSlice, TranscriptReaderError> {
    session_component(native_id)?;
    for app_dir in roots {
        if let Some(path) = transcript_path_for(app_dir, native_id)? {
            let text = read_uncapped(&path)?;
            return capture::map_transcript(native_id, &path, &text, offset);
        }
    }
    Ok(TranscriptSlice::empty(offset))
}

fn nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn first_workspace_path(raw: &str) -> Option<String> {
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let uris = serde_json::from_str::<Vec<String>>(raw).ok()?;
    uris.into_iter().find_map(|uri| {
        let stripped = uri.strip_prefix("file://").unwrap_or(&uri).trim();
        (!stripped.is_empty()).then(|| stripped.to_owned())
    })
}
