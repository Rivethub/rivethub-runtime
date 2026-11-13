mod capture;
mod model;

use std::io::Read as _;
use std::path::Path;
use std::path::PathBuf;

use kernel::transcript::TranscriptReaderError;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use super::message::TranscriptSlice;
use super::message::blocking;
use super::message::native;
use super::message::report;
use crate::store_path::DirState;
use crate::store_path::Root;
use crate::store_path::dir_state;
use crate::store_path::is_not_found;
use crate::store_path::session_component;
use crate::transcript::read_uncapped;

const FORMAT: &str = "dsh-session";

#[derive(Debug)]
pub(crate) struct DshTranscriptReader {
    root: PathBuf,
}

impl DshTranscriptReader {
    #[must_use]
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) async fn capture(
        &self,
        session: &SessionId,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        let Some(native_id) = native(session, HarnessId::Dsh).map(str::to_owned) else {
            return Ok(TranscriptSlice::empty(0));
        };
        let root = self.root.clone();
        let slice = blocking(move || match transcript(&root, &native_id)? {
            Some((path, text)) => capture::map_session(&native_id, &path, &text),
            None => Ok(TranscriptSlice::empty(0)),
        })
        .await?;
        report(FORMAT, &slice.skipped);
        Ok(slice)
    }
}

pub(crate) async fn title(
    root: PathBuf,
    id: String,
) -> Result<Option<String>, TranscriptReaderError> {
    blocking(move || match transcript(&root, &id)? {
        Some((_, text)) => capture::session_title(&text),
        None => Ok(None),
    })
    .await
}

pub(crate) fn sessions(root: &Path) -> Result<Vec<(String, PathBuf)>, TranscriptReaderError> {
    let Some((root, workspaces)) = workspaces(root)? else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for workspace in workspaces {
        for folder in root.directories(&workspace)? {
            let Some(id) = folder.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if session_component(id).is_err() {
                continue;
            }
            if let Some(path) = newest_transcript(&root, &folder)? {
                found.push((id.to_owned(), path));
            }
        }
    }
    Ok(found)
}

fn transcript(
    root: &Path,
    native_id: &str,
) -> Result<Option<(PathBuf, String)>, TranscriptReaderError> {
    session_component(native_id)?;
    let Some((root, workspaces)) = workspaces(root)? else {
        return Ok(None);
    };
    for workspace in workspaces {
        let folder = workspace.join(native_id);
        if dir_state(&root, &folder)? == DirState::Absent {
            continue;
        }
        if let Some(path) = newest_transcript(&root, &folder)? {
            let text = decode(&path)?;
            return Ok(Some((path, text)));
        }
    }
    Ok(None)
}

fn workspaces(root: &Path) -> Result<Option<(Root, Vec<PathBuf>)>, TranscriptReaderError> {
    let root = match Root::open(root) {
        Ok(root) => root,
        Err(error) if is_not_found(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    let sessions = root.canonical().join("sessions");
    if dir_state(&root, &sessions)? == DirState::Absent {
        return Ok(None);
    }
    let workspaces = root.directories(&sessions)?;
    Ok(Some((root, workspaces)))
}

fn newest_transcript(root: &Root, folder: &Path) -> Result<Option<PathBuf>, TranscriptReaderError> {
    Ok(root
        .files(folder)?
        .into_iter()
        .rfind(|path| transcript_name(path)))
}

fn transcript_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let plain = name
        .strip_suffix(".zstd")
        .or_else(|| name.strip_suffix(".zst"))
        .unwrap_or(name);
    let Some(version) = plain
        .strip_suffix(".jsonl")
        .and_then(|stem| stem.strip_prefix("session"))
    else {
        return false;
    };
    version.is_empty()
        || version.strip_prefix('.').is_some_and(|version| {
            !version.is_empty()
                && version
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn decode(path: &Path) -> Result<String, TranscriptReaderError> {
    let compressed = path
        .extension()
        .is_some_and(|extension| extension == "zstd" || extension == "zst");
    if !compressed {
        return read_uncapped(path);
    }
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path)
        .and_then(zstd::stream::read::Decoder::new)
        .and_then(|mut decoder| decoder.read_to_end(&mut bytes));
    if let Err(error) = read {
        let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
            return Err(TranscriptReaderError::Read {
                message: format!("dsh transcript {}: {error}", path.display()),
                source: Box::new(error),
            });
        };
        tracing::warn!(
            "dsh transcript {} ends in a frame still being written: {error}",
            path.display()
        );
        bytes.truncate(end + 1);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
