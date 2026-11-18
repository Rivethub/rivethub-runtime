use jiff::Timestamp;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;
use values::time::TimestampFormat as _;

use super::MemoryEntry;

pub const SESSION_MESSAGES_DEFAULT: u32 = 200;
pub const SESSION_MESSAGES_MAX: u32 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cursor {0:?} is not one this server issued")]
pub struct CursorError(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MessageCursor {
    pub created_at: Timestamp,
    pub id: Uuid,
    prefix: Option<[u8; 32]>,
}

impl MessageCursor {
    #[must_use]
    pub const fn new(created_at: Timestamp, id: Uuid) -> Self {
        Self {
            created_at,
            id,
            prefix: None,
        }
    }

    #[must_use]
    pub fn encode(&self) -> String {
        let key = format!("{}.{}", self.created_at.epoch_micros(), self.id);
        match self.prefix {
            Some(prefix) => format!("{key}.{}", hex::encode(prefix)),
            None => key,
        }
    }

    pub fn decode(text: &str) -> Result<Self, CursorError> {
        let invalid = || CursorError(text.to_owned());
        let (micros, id) = text.split_once('.').ok_or_else(invalid)?;
        let micros: i64 = micros.parse().map_err(|_| invalid())?;
        let (id, prefix) = match id.split_once('.') {
            Some((id, encoded)) => {
                let mut prefix = [0; 32];
                hex::decode_to_slice(encoded, &mut prefix).map_err(|_| invalid())?;
                (id, Some(prefix))
            }
            None => (id, None),
        };
        Ok(Self {
            created_at: Timestamp::from_nanosecond(i128::from(micros) * 1_000)
                .map_err(|_error| invalid())?,
            id: Uuid::parse_str(id).map_err(|_| invalid())?,
            prefix,
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

    /// Returns the page's offset, its cursors and one lookahead cursor if available.
    /// The next cursor commits to every ID already seen in transcript order.
    pub fn positioned_page(
        &self,
        mut ordered: impl ExactSizeIterator<Item = MessageCursor>,
    ) -> Result<(usize, Vec<MessageCursor>), CursorError> {
        let mut digest = Sha256::new();
        let start = match self.after {
            None => 0,
            Some(after) => {
                let invalid = || CursorError(after.encode());
                let expected = after.prefix.ok_or_else(invalid)?;
                let total = ordered.len();
                loop {
                    let cursor = ordered.next().ok_or_else(invalid)?;
                    digest.update(cursor.id.as_bytes());
                    if cursor.id == after.id {
                        break;
                    }
                }
                let actual: [u8; 32] = digest.clone().finalize().into();
                if actual != expected {
                    return Err(invalid());
                }
                total - ordered.len()
            }
        };
        let mut page: Vec<_> = ordered.take(self.limit as usize + 1).collect();
        if page.len() > self.limit as usize
            && let Some((last, preceding)) = page
                .get_mut(..self.limit as usize)
                .and_then(<[_]>::split_last_mut)
        {
            for cursor in preceding.iter().chain(std::iter::once(&*last)) {
                digest.update(cursor.id.as_bytes());
            }
            last.prefix = Some(digest.finalize().into());
        }
        Ok((start, page))
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
