mod ordinal;
mod output;
mod subagent;
mod turns;
pub(crate) mod updates;

use jiff::Timestamp;
use std::path::Path;
use std::path::PathBuf;

use async_trait::async_trait;
use kernel::transcript::TranscriptReader;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::Turn;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use super::message::TranscriptSlice;
use super::message::blocking;
use super::message::native;
use super::message::project_turns;
use super::message::report;
use super::scan::read_optional;
use super::scan::read_whole;
use crate::store_path::Child;
use crate::store_path::DirState;
use crate::store_path::Root;
use crate::store_path::dir_state;
use crate::store_path::session_component;

pub(crate) use output::tool_result;
use updates::Extent;
use updates::UpdateLines;

pub(crate) const FORMAT: &str = "grok-updates";
const UPDATES: &str = "updates.jsonl";

#[derive(Debug)]
pub struct GrokTranscriptReader {
    root: PathBuf,
}

impl GrokTranscriptReader {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub async fn messages(
        &self,
        session: &SessionId,
        offset: u64,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        self.slice(session, offset, Extent::Settled, Reach::Capped)
            .await
    }

    pub async fn capture(
        &self,
        session: &SessionId,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        self.slice(session, 0, Extent::Live, Reach::Whole).await
    }

    async fn slice(
        &self,
        session: &SessionId,
        offset: u64,
        extent: Extent,
        reach: Reach,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        let Some(native_id) = native(session, HarnessId::GrokBuild).map(str::to_owned) else {
            return Ok(TranscriptSlice::empty(offset));
        };
        let root = self.root.clone();
        let session = session.clone();
        let slice = blocking(move || {
            let text = updates_text(&root, &native_id, reach)?;
            text.map_or_else(
                || Ok(TranscriptSlice::empty(offset)),
                |text| updates::map_updates(&session, &text, offset, extent),
            )
        })
        .await?;
        report(FORMAT, &slice.skipped);
        Ok(slice)
    }
}

#[async_trait]
impl TranscriptReader for GrokTranscriptReader {
    async fn sessions(&self, store: &Path) -> Result<Vec<SessionId>, TranscriptReaderError> {
        let store = store.to_path_buf();
        blocking(move || list_sessions(&store)).await
    }

    async fn read(&self, session: &SessionId) -> Result<Vec<Turn>, TranscriptReaderError> {
        Ok(project_turns(
            &self.slice(session, 0, Extent::Live, Reach::Capped).await?,
        ))
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

pub(crate) async fn subagent_link(
    store: PathBuf,
    native_id: String,
) -> Result<Option<crate::records::SubagentLink>, TranscriptReaderError> {
    blocking(move || subagent::link(&store, &native_id)).await
}

fn list_sessions(store: &Path) -> Result<Vec<SessionId>, TranscriptReaderError> {
    let mut sessions = Vec::new();
    for (id, _) in session_dirs(&Root::open(store)?)? {
        sessions.push(SessionId::new(HarnessId::GrokBuild, &id));
    }
    sessions.sort();
    sessions.dedup();
    Ok(sessions)
}

pub(crate) fn update_lines(
    store: &Path,
    native_id: &str,
) -> Result<UpdateLines, TranscriptReaderError> {
    match updates_text(store, native_id, Reach::Capped)? {
        Some(text) => updates::parse_lines(&text),
        None => Ok(UpdateLines::default()),
    }
}

#[derive(Debug, Clone, Copy)]
enum Reach {
    Capped,
    Whole,
}

fn updates_text(
    store: &Path,
    native_id: &str,
    reach: Reach,
) -> Result<Option<String>, TranscriptReaderError> {
    session_component(native_id)?;
    let root = Root::open(store)?;
    let Some(dir) = session_dirs(&root)?
        .into_iter()
        .find_map(|(id, path)| (id == native_id).then_some(path))
    else {
        return Ok(None);
    };
    match reach {
        Reach::Capped => read_optional(&root, &dir.join(UPDATES)),
        Reach::Whole => read_whole(&root, &dir.join(UPDATES)),
    }
}

fn session_dirs(root: &Root) -> Result<Vec<(String, PathBuf)>, TranscriptReaderError> {
    let sessions = root.canonical().join("sessions");
    if dir_state(root, &sessions)? == DirState::Absent {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for encoded in root.directories(&sessions)? {
        for session_dir in root.directories(&encoded)? {
            let name = utf8_dir_name(&session_dir)?;
            if uuid::Uuid::parse_str(&name).is_err() {
                continue;
            }
            if root.classify(&session_dir.join(UPDATES))? == Child::File {
                found.push((name, session_dir));
            }
        }
    }
    Ok(found)
}

fn utf8_dir_name(path: &Path) -> Result<String, TranscriptReaderError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| crate::store_path::refused(path, "directory name is not utf-8"))
}
