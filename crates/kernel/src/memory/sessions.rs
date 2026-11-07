use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use values::HarnessId;
use values::SessionId;

use super::MemoryStoreError;
use super::names_session;
use crate::topics::TopicId;

pub const SESSION_PAGE_DEFAULT: u32 = 50;
pub const SESSION_PAGE_MAX: u32 = 200;
const SESSION_SNIPPET_CHARS: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, strum::Display)]
#[strum(serialize_all = "lowercase")]
pub enum SessionView {
    #[default]
    All,
    Unfiled,
    Kept,
    Trash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryState {
    Capturing,
    Queued,
    Summarized,
    Flagged,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionCursor {
    pub started_at: DateTime<Utc>,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionQuery {
    pub view: SessionView,
    pub harness: Option<HarnessId>,
    pub topic: Option<TopicId>,
    pub text: Option<String>,
    pub after: Option<SessionCursor>,
    pub exclude: Vec<String>,
    pub limit: u32,
}

impl SessionQuery {
    #[must_use]
    pub const fn new(view: SessionView) -> Self {
        Self {
            view,
            harness: None,
            topic: None,
            text: None,
            after: None,
            exclude: Vec::new(),
            limit: SESSION_PAGE_DEFAULT,
        }
    }

    #[must_use]
    pub fn page_size(&self) -> u32 {
        self.limit.clamp(1, SESSION_PAGE_MAX)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTopic {
    pub id: TopicId,
    pub path: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub title: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub message_count: u64,
    pub topic: Option<SessionTopic>,
    pub kept: bool,
    pub trashed: bool,
    pub memory: MemoryState,
    pub gist: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionPage {
    pub items: Vec<SessionRecord>,
    pub next: Option<SessionCursor>,
}

#[async_trait]
pub trait SessionCatalog: Send + Sync {
    async fn list(&self, query: &SessionQuery) -> Result<SessionPage, MemoryStoreError>;

    async fn get(&self, session: &SessionId) -> Result<Option<SessionRecord>, MemoryStoreError>;
}

#[must_use]
pub fn snippet(text: &str) -> Option<String> {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    let mut chars = flat.chars();
    let head: String = chars.by_ref().take(SESSION_SNIPPET_CHARS).collect();
    Some(if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    })
}

#[must_use]
pub const fn memory_state(active: bool, flagged: bool, summarized: bool) -> MemoryState {
    if flagged {
        MemoryState::Flagged
    } else if active {
        MemoryState::Capturing
    } else if summarized {
        MemoryState::Summarized
    } else {
        MemoryState::Queued
    }
}

const SESSION_TITLE_CHARS: usize = 80;

fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

fn shorten_paths(line: &str) -> String {
    line.split_whitespace()
        .map(|word| {
            let bare =
                word.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '(' | ')' | ',' | ';'));
            let rooted = bare.starts_with('/') || bare.starts_with("~/");
            match bare.trim_end_matches('/').rsplit('/').next() {
                Some(last) if rooted && !last.is_empty() => last,
                _ if rooted => "",
                _ => word,
            }
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn clip(text: &str) -> Option<String> {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(SESSION_TITLE_CHARS).collect();
    let head = head.trim_end().to_owned();
    if head.is_empty() {
        return None;
    }
    Some(if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    })
}

#[must_use]
pub fn session_title(
    title: Option<&str>,
    summary: Option<&str>,
    first_user: Option<&str>,
    native: &str,
) -> String {
    let named = title
        .map(str::trim)
        .filter(|title| !title.is_empty() && *title != native && names_session(title));
    if let Some(title) = named {
        return title.to_owned();
    }
    let derived = [summary, first_user]
        .into_iter()
        .flatten()
        .filter_map(first_line)
        .find_map(|line| clip(&shorten_paths(line)));
    let Some(derived) = derived else {
        return native.to_owned();
    };
    derived
}
