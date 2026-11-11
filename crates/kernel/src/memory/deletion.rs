use std::collections::BTreeMap;
use std::ops::AddAssign;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;

use super::MemoryStoreError;
use super::names_session;

const TRASH_MIN_AGE: TimeDelta = TimeDelta::hours(1);
const TRASH_REPLY_MAX_CHARS: usize = 80;
const TRASH_PROMPT_MAX_CHARS: usize = 280;
pub const TOMBSTONE_PURGE_BATCH: u32 = 100;
pub const TRASH_PURGE_BATCH: u32 = 100;
pub const TRASH_RETENTION_DAYS: u32 = 30;
pub const EMPTY_SESSIONS_MAX: u32 = 500;
pub const TRASH_MESSAGES_MAX: u64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeletedSession {
    pub conversations: u64,
    pub messages: u64,
    pub summaries: u64,
}

impl AddAssign for DeletedSession {
    fn add_assign(&mut self, other: Self) {
        self.conversations += other.conversations;
        self.messages += other.messages;
        self.summaries += other.summaries;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionActivity {
    pub conversations: u64,
    pub messages: u64,
    pub user_messages: u64,
    pub assistant_messages: u64,
    pub reply: Option<String>,
    pub tool_rows: u64,
    pub attachments: u64,
    pub tasked: bool,
    pub placed: bool,
    pub cited: bool,
}

impl SessionActivity {
    #[must_use]
    pub fn recorded(&self) -> bool {
        self.conversations > 0 || self.messages > 0
    }

    #[must_use]
    pub fn kept(&self) -> bool {
        self.messages > TRASH_MESSAGES_MAX
            || self.tool_rows > 0
            || self.attachments > 0
            || self.tasked
            || self.placed
            || self.cited
    }
}

#[async_trait]
pub trait SessionDeletion: Send + Sync {
    async fn delete_session(
        &self,
        session_key: &str,
        deleted_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<DeletedSession, MemoryStoreError>;

    async fn tombstones(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, MemoryStoreError>;

    async fn session_activity(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, SessionActivity>, MemoryStoreError>;

    async fn empty_sessions(&self, limit: u32) -> Result<Vec<String>, MemoryStoreError>;

    async fn purge_tombstoned(&self, limit: u32) -> Result<DeletedSession, MemoryStoreError>;

    async fn keep_session(
        &self,
        session_key: &str,
        kept_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), MemoryStoreError>;

    async fn kept(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, MemoryStoreError>;

    async fn unkeep_session(&self, session_key: &str) -> Result<(), MemoryStoreError>;

    async fn trash_session(
        &self,
        session_key: &str,
        trashed_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), MemoryStoreError>;

    async fn restore_session(&self, session_key: &str) -> Result<(), MemoryStoreError>;

    async fn trashed(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, MemoryStoreError>;

    async fn purge_trashed(
        &self,
        older_than: DateTime<Utc>,
        limit: u32,
    ) -> Result<DeletedSession, MemoryStoreError>;
}

#[must_use]
pub fn used_since(
    deleted_at: DateTime<Utc>,
    stamps: impl IntoIterator<Item = Option<DateTime<Utc>>>,
) -> bool {
    stamps.into_iter().flatten().any(|stamp| stamp > deleted_at)
}

fn placeholder_title(title: Option<&str>, native: &str) -> bool {
    title.is_none_or(|title| title.trim() == native || !names_session(title))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpokenTurn<'a> {
    pub speaker: Speaker,
    pub text: &'a str,
    pub worked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrashReason {
    Empty,
    OneWordReply,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashSignals<'a> {
    pub native: &'a str,
    pub title: Option<&'a str>,
    pub updated_at: DateTime<Utc>,
    pub now: DateTime<Utc>,
    pub activity: &'a SessionActivity,
    pub transcript: &'a [SpokenTurn<'a>],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashVerdict {
    pub reason: TrashReason,
    pub user_turns: u64,
    pub reply: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Turns {
    user: u64,
    assistant: u64,
    reply: Option<String>,
    prompt_chars: usize,
    kept: bool,
}

impl Turns {
    fn of_transcript(transcript: &[SpokenTurn<'_>]) -> Self {
        let mut turns = Self::default();
        for turn in transcript {
            turns.kept |= turn.worked;
            match turn.speaker {
                Speaker::User => {
                    let prompt = turn.text.trim();
                    if !prompt.is_empty() {
                        turns.user += 1;
                        turns.prompt_chars = turns.prompt_chars.max(prompt.chars().count());
                    }
                }
                Speaker::Assistant => {
                    turns.assistant += 1;
                    if turns.reply.is_none() {
                        turns.reply = Some(turn.text.to_owned());
                    }
                }
            }
        }
        turns
    }

    const fn silent(&self) -> bool {
        self.user == 0 && self.assistant == 0
    }

    fn brief(&self) -> bool {
        self.user <= 1 && self.assistant <= 1 && self.reply.as_deref().is_none_or(one_word)
    }

    fn of_memory(activity: &SessionActivity) -> Self {
        Self {
            user: activity.user_messages,
            assistant: activity.assistant_messages,
            reply: activity.reply.clone(),
            prompt_chars: 0,
            kept: activity.kept(),
        }
    }
}

fn one_word(text: &str) -> bool {
    let trimmed = text.trim_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation());
    !trimmed.chars().any(char::is_whitespace)
}

fn preview(reply: Option<&str>) -> Option<String> {
    let text = reply.map(str::trim).filter(|text| !text.is_empty())?;
    Some(text.chars().take(TRASH_REPLY_MAX_CHARS).collect())
}

#[must_use]
pub fn trash_verdict(signals: &TrashSignals<'_>) -> Option<TrashVerdict> {
    if signals.now.signed_duration_since(signals.updated_at) < TRASH_MIN_AGE {
        return None;
    }
    let memory = Turns::of_memory(signals.activity);
    let transcript = Turns::of_transcript(signals.transcript);
    if memory.kept || transcript.kept {
        return None;
    }
    if memory.silent() && transcript.silent() {
        return placeholder_title(signals.title, signals.native).then_some(TrashVerdict {
            reason: TrashReason::Empty,
            user_turns: 0,
            reply: None,
        });
    }
    let user_turns = memory.user.max(transcript.user);
    let reply = transcript.reply.as_deref().or(memory.reply.as_deref());
    let answered = reply.is_some_and(|reply| !reply.trim().is_empty());
    let brief = user_turns == 1
        && answered
        && transcript.prompt_chars <= TRASH_PROMPT_MAX_CHARS
        && memory.brief()
        && transcript.brief();
    brief.then(|| TrashVerdict {
        reason: TrashReason::OneWordReply,
        user_turns,
        reply: preview(reply),
    })
}
