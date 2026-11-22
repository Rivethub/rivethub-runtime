use std::collections::BTreeSet;

use jiff::Timestamp;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::summary_order::{MessagePosition, order_messages};
use kernel::wiki::entity_pass::{EntityLeaf, EntityStoreError, TranscriptMessage};
use rusqlite::{Connection, Row, params};
use uuid::Uuid;
use wire::{JsonObject, message::Role};

use super::{at, error, rows, uuid};
use crate::message::{stored_role, transcript_metadata};
use crate::time::seek_key;

struct Entry {
    conversation: Uuid,
    at: Timestamp,
    role: Role,
    metadata: Option<JsonObject>,
    message: TranscriptMessage,
}

pub(super) fn load(
    conn: &Connection,
    leaves: &[EntityLeaf],
) -> Result<Vec<TranscriptMessage>, EntityStoreError> {
    let mut wanted = BTreeSet::new();
    for leaf in leaves {
        if let Some(delivery) = &leaf.delivery {
            wanted.extend(delivery.pending.iter().map(Uuid::to_string));
        } else {
            wanted.extend(linked_or_window(conn, leaf)?);
        }
    }
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let wanted = serde_json::to_string(&wanted).map_err(error)?;
    let entries = rows(
        conn,
        "SELECT m.id, m.conversation_id, m.created_at, m.role, m.metadata, m.tool_name,
                m.content, nullif(m.tool_args, 'null'), m.tool_result
         FROM ros_messages m JOIN json_each(?1) q ON q.value = m.id
         ORDER BY rh_instant(m.created_at), m.id",
        [wanted],
        RawEntry::read,
    )?
    .into_iter()
    .map(RawEntry::decode)
    .collect::<Result<Vec<_>, _>>()?;
    Ok(order_messages(entries, MemoryBrowseOrder::Asc, |entry| {
        MessagePosition::new(entry.at, entry.role, entry.metadata.as_ref())
            .in_conversation(entry.conversation)
    })
    .into_iter()
    .map(|entry| entry.message)
    .collect())
}

fn linked_or_window(conn: &Connection, leaf: &EntityLeaf) -> Result<Vec<String>, EntityStoreError> {
    let linked = rows(
        conn,
        "SELECT message_id FROM ros_summary_sources WHERE summary_id = ?1",
        [leaf.id.to_string()],
        |row| row.get(0),
    )?;
    let Some(conversation) = leaf.conversation_id.filter(|_| linked.is_empty()) else {
        return Ok(linked);
    };
    rows(
        conn,
        "SELECT id FROM rh_transcript_times
               WHERE conversation_id = ?1 AND seek_time BETWEEN ?2 AND ?3",
        params![
            conversation.to_string(),
            seek_key(leaf.start),
            seek_key(leaf.end)
        ],
        |row| row.get(0),
    )
}

struct RawEntry {
    id: String,
    conversation: String,
    at: String,
    role: Role,
    metadata: String,
    tool_name: Option<String>,
    content: String,
    tool_args: Option<String>,
    tool_result: Option<String>,
}

impl RawEntry {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            conversation: row.get(1)?,
            at: row.get(2)?,
            role: stored_role(row, 3)?,
            metadata: row.get(4)?,
            tool_name: row.get(5)?,
            content: row.get(6)?,
            tool_args: row.get(7)?,
            tool_result: row.get(8)?,
        })
    }

    fn decode(self) -> Result<Entry, EntityStoreError> {
        let at = at(&self.at)?;
        let id = uuid(&self.id)?;
        Ok(Entry {
            conversation: uuid(&self.conversation)?,
            at,
            role: self.role,
            metadata: transcript_metadata(&self.metadata),
            message: TranscriptMessage {
                id,
                at,
                role: self.role.to_string(),
                tool_name: self.tool_name.filter(|name| !name.trim().is_empty()),
                content: self.content,
                tool_args: self
                    .tool_args
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(error)?,
                tool_result: self.tool_result.filter(|text| !text.is_empty()),
            },
        })
    }
}
