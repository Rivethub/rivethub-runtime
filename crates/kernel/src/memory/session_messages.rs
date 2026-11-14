use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::MemoryEntry;

pub const SESSION_MESSAGES_DEFAULT: u32 = 200;
pub const SESSION_MESSAGES_MAX: u32 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cursor {0:?} is not one this server issued")]
pub struct CursorError(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageCursor {
    pub created_at: DateTime<Utc>,
    pub id: Uuid,
}

impl MessageCursor {
    #[must_use]
    pub fn encode(&self) -> String {
        format!("{}.{}", self.created_at.timestamp_micros(), self.id)
    }

    pub fn decode(text: &str) -> Result<Self, CursorError> {
        let invalid = || CursorError(text.to_owned());
        let (micros, id) = text.split_once('.').ok_or_else(invalid)?;
        let micros: i64 = micros.parse().map_err(|_| invalid())?;
        Ok(Self {
            created_at: DateTime::from_timestamp_micros(micros).ok_or_else(invalid)?,
            id: Uuid::parse_str(id).map_err(|_| invalid())?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagesQuery {
    pub after: Option<MessageCursor>,
    pub limit: u32,
}

impl MessagesQuery {
    #[must_use]
    pub fn new(after: Option<MessageCursor>, limit: Option<u32>) -> Self {
        Self {
            after,
            limit: limit
                .unwrap_or(SESSION_MESSAGES_DEFAULT)
                .clamp(1, SESSION_MESSAGES_MAX),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionMessages {
    pub items: Vec<MemoryEntry>,
    pub next: Option<MessageCursor>,
    pub total: u64,
}

impl SessionMessages {
    #[must_use]
    pub fn page(mut rows: Vec<(MemoryEntry, MessageCursor)>, limit: u32, total: u64) -> Self {
        let limit = limit as usize;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next = more
            .then(|| rows.last().map(|(_, cursor)| *cursor))
            .flatten();
        Self {
            items: rows.into_iter().map(|(entry, _)| entry).collect(),
            next,
            total,
        }
    }
}
