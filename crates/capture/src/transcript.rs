use std::path::Path;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::transcript::TranscriptReader;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::Turn;
use serde::Deserialize;
use wire::harness::HarnessId;
use wire::message::Role;
use wire::session_id::SessionId;

use crate::event_id::ToolInput;
use crate::store_path::Child;
use crate::store_path::DirState;
use crate::store_path::Root;
use crate::store_path::dir_state;
use crate::store_path::jsonl;
use crate::store_path::session_component;
use crate::store_path::single_component;
use crate::store_path::utf8_stem;

const TRANSCRIPT_MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineKind {
    User,
    Assistant,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineMessage {
    pub role: Role,
    pub content: LineContent,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum LineContent {
    Text(String),
    Blocks(Vec<Block>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: ToolInput,
    },
    ToolResult {
        tool_use_id: Option<String>,
        content: LineContent,
    },
    Thinking {
        thinking: Option<String>,
        text: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    #[serde(skip)]
    pub number: usize,
    pub r#type: LineKind,
    pub uuid: Option<String>,
    pub timestamp: Option<String>,
    pub message: Option<LineMessage>,
    pub is_sidechain: Option<bool>,
    pub is_meta: Option<bool>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ToolUse<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub input: &'a ToolInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub tool_use_id: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedLine {
    pub number: usize,
    pub reason: String,
}

#[derive(Debug, Default, Clone)]
pub struct Parsed {
    pub lines: Vec<Line>,
    pub skipped: Vec<SkippedLine>,
}

#[derive(Debug, Default, Clone)]
pub struct Turns {
    pub turns: Vec<Turn>,
    pub skipped: Vec<SkippedLine>,
}

impl Line {
    #[must_use]
    pub fn text(&self) -> Option<String> {
        let message = self.message.as_ref()?;
        match &message.content {
            LineContent::Text(text) => Some(text.clone()),
            LineContent::Blocks(blocks) => Some(block_texts(blocks)),
        }
    }

    #[must_use]
    pub fn tool_result_text(&self) -> Option<String> {
        let message = self.message.as_ref()?;
        let LineContent::Blocks(blocks) = &message.content else {
            return None;
        };
        blocks.iter().find_map(|block| match block {
            Block::ToolResult { content, .. } => match content {
                LineContent::Text(text) => Some(text.clone()),
                LineContent::Blocks(nested) => {
                    let text = block_texts(nested);
                    (!text.is_empty()).then_some(text)
                }
            },
            Block::Text { .. } | Block::ToolUse { .. } | Block::Thinking { .. } | Block::Other => {
                None
            }
        })
    }

    #[must_use]
    pub fn tool_uses(&self) -> Vec<ToolUse<'_>> {
        let Some(blocks) = self.message_blocks() else {
            return Vec::new();
        };
        blocks
            .iter()
            .filter_map(|block| match block {
                Block::ToolUse { id, name, input } => Some(ToolUse { id, name, input }),
                Block::Text { .. }
                | Block::ToolResult { .. }
                | Block::Thinking { .. }
                | Block::Other => None,
            })
            .collect()
    }

    #[must_use]
    pub fn tool_results(&self) -> Vec<ToolOutcome> {
        let Some(blocks) = self.message_blocks() else {
            return Vec::new();
        };
        blocks
            .iter()
            .filter_map(|block| match block {
                Block::ToolResult {
                    tool_use_id,
                    content,
                } => {
                    let text = match content {
                        LineContent::Text(text) => text.clone(),
                        LineContent::Blocks(nested) => block_texts(nested),
                    };
                    Some(ToolOutcome {
                        tool_use_id: tool_use_id.clone(),
                        text,
                    })
                }
                Block::Text { .. }
                | Block::ToolUse { .. }
                | Block::Thinking { .. }
                | Block::Other => None,
            })
            .collect()
    }

    fn message_blocks(&self) -> Option<&[Block]> {
        let message = self.message.as_ref()?;
        match &message.content {
            LineContent::Blocks(blocks) => Some(blocks),
            LineContent::Text(_) => None,
        }
    }
}

fn block_texts(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        if let Block::Text { text } = block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
}

#[must_use]
pub fn parse_lines(text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    for (offset, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Line>(line) {
            Ok(line) => parsed.lines.push(Line {
                number: offset + 1,
                ..line
            }),
            Err(error) => parsed.skipped.push(SkippedLine {
                number: offset + 1,
                reason: error.to_string(),
            }),
        }
    }
    parsed
}

pub fn report_skipped(skipped: &[SkippedLine]) {
    if skipped.is_empty() {
        return;
    }
    let numbers: Vec<usize> = skipped.iter().map(|line| line.number).collect();
    tracing::warn!(
        count = skipped.len(),
        lines = ?numbers,
        "transcript lines are not valid claude JSONL and were skipped"
    );
}

pub fn read_capped(path: &Path) -> Result<String, TranscriptReaderError> {
    let bytes = std::fs::read(path).map_err(|error| read_error(path, error))?;
    Ok(capped_text(&bytes))
}

pub(crate) fn read_uncapped(path: &Path) -> Result<String, TranscriptReaderError> {
    let bytes = std::fs::read(path).map_err(|error| read_error(path, error))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) async fn read_uncapped_async(path: &Path) -> Result<String, TranscriptReaderError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|error| read_error(path, error))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub async fn read_capped_async(path: &Path) -> Result<String, TranscriptReaderError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|error| read_error(path, error))?;
    Ok(capped_text(&bytes))
}

fn read_error(path: &Path, error: std::io::Error) -> TranscriptReaderError {
    TranscriptReaderError::Read {
        message: format!("transcript {}: {error}", path.display()),
        source: Box::new(error),
    }
}

#[expect(
    clippy::indexing_slicing,
    reason = "the over-cap branch slices at TRANSCRIPT_MAX_BYTES, proven below the length, and the cut at a line end is at most the length"
)]
fn capped_text(bytes: &[u8]) -> String {
    if bytes.len() <= TRANSCRIPT_MAX_BYTES {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let limited = &bytes[..TRANSCRIPT_MAX_BYTES];
    let end = limited
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(limited.len(), |position| position + 1);
    String::from_utf8_lossy(&limited[..end]).into_owned()
}

#[must_use]
pub fn native_id(session: &SessionId) -> Option<&str> {
    (session.harness() == HarnessId::ClaudeCode).then(|| session.native())
}

#[derive(Debug)]
pub struct ClaudeTranscriptReader {
    store_root: PathBuf,
}

impl ClaudeTranscriptReader {
    #[must_use]
    pub fn new(store_root: PathBuf) -> Self {
        Self { store_root }
    }

    pub async fn capture_messages(
        &self,
        session: &SessionId,
        offset: u64,
    ) -> Result<crate::readers::TranscriptSlice, TranscriptReaderError> {
        let Some(native) = native_id(session).map(str::to_owned) else {
            return Ok(crate::readers::TranscriptSlice::empty(offset));
        };
        let root = self.store_root.clone();
        let text = tokio::task::spawn_blocking(move || find_transcript(&root, &native))
            .await
            .map_err(|error| TranscriptReaderError::Unavailable(Box::new(error)))??;
        let parsed = parse_lines(&text);
        let slice = crate::readers::from_parsed(session, &parsed, offset)?;
        crate::readers::report("claude-jsonl", &slice.skipped);
        Ok(slice)
    }
}

#[must_use]
pub fn turns_of(lines: &[Line]) -> Turns {
    let mut turns = Turns::default();
    for line in lines {
        if line.is_meta == Some(true) {
            continue;
        }
        if line.r#type == LineKind::Other {
            turns.skipped.push(SkippedLine {
                number: line.number,
                reason: "unknown record type".to_owned(),
            });
            continue;
        }
        let Some(text) = line.timestamp.as_deref() else {
            continue;
        };
        let at = match DateTime::parse_from_rfc3339(text) {
            Ok(at) => at.with_timezone(&Utc),
            Err(error) => {
                turns.skipped.push(SkippedLine {
                    number: line.number,
                    reason: format!("timestamp {text:?} is not an RFC 3339 datetime: {error}"),
                });
                continue;
            }
        };
        if let Some((role, content)) = turn_content(line) {
            turns.turns.push(Turn { role, content, at });
        }
    }
    turns
}

fn turn_content(line: &Line) -> Option<(Role, String)> {
    let (role, content) = match line.r#type {
        LineKind::User => match line.tool_result_text() {
            Some(result) => (Role::Tool, result),
            None => (Role::User, line.text()?),
        },
        LineKind::Assistant => (Role::Assistant, line.text()?),
        LineKind::Other => return None,
    };
    (!content.is_empty()).then_some((role, content))
}

#[async_trait]
impl TranscriptReader for ClaudeTranscriptReader {
    async fn sessions(&self, store: &Path) -> Result<Vec<SessionId>, TranscriptReaderError> {
        let store = store.to_path_buf();
        tokio::task::spawn_blocking(move || sessions_blocking(&store))
            .await
            .map_err(|error| TranscriptReaderError::Unavailable(Box::new(error)))?
    }

    async fn read(&self, session: &SessionId) -> Result<Vec<Turn>, TranscriptReaderError> {
        let Some(native) = native_id(session).map(str::to_owned) else {
            return Ok(Vec::new());
        };
        let root = self.store_root.clone();
        let text = tokio::task::spawn_blocking(move || find_transcript(&root, &native))
            .await
            .map_err(|error| TranscriptReaderError::Unavailable(Box::new(error)))??;
        let parsed = parse_lines(&text);
        let turns = turns_of(&parsed.lines);
        report_skipped(&[parsed.skipped, turns.skipped].concat());
        Ok(turns.turns)
    }

    async fn tail(
        &self,
        session: &SessionId,
        since: DateTime<Utc>,
    ) -> Result<Vec<Turn>, TranscriptReaderError> {
        Ok(self
            .read(session)
            .await?
            .into_iter()
            .filter(|turn| turn.at > since)
            .collect())
    }
}

fn sessions_blocking(store: &Path) -> Result<Vec<SessionId>, TranscriptReaderError> {
    let root = Root::open(store)?;
    let projects = root.canonical().join("projects");
    if dir_state(&root, &projects)? == DirState::Absent {
        return Err(missing_projects(&projects));
    }
    let mut sessions = Vec::new();
    for directory in root.directories(&projects)? {
        for path in root.files(&directory)? {
            if !jsonl(&path) {
                continue;
            }
            let stem = utf8_stem(&path)?;
            if !single_component(&stem) {
                continue;
            }
            sessions.push(SessionId::new(HarnessId::ClaudeCode, &stem));
        }
    }
    sessions.sort();
    sessions.dedup();
    Ok(sessions)
}

fn find_transcript(root: &Path, native: &str) -> Result<String, TranscriptReaderError> {
    session_component(native)?;
    let store = Root::open(root)?;
    let projects = store.canonical().join("projects");
    if dir_state(&store, &projects)? == DirState::Absent {
        return Err(missing_projects(&projects));
    }
    for directory in store.directories(&projects)? {
        let candidate = directory.join(format!("{native}.jsonl"));
        if store.classify(&candidate)? == Child::File {
            return read_capped(&candidate);
        }
    }
    Ok(String::new())
}

fn missing_projects(path: &Path) -> TranscriptReaderError {
    TranscriptReaderError::Unavailable(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("projects directory {} is missing", path.display()),
    )))
}
