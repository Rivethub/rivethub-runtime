use std::str::FromStr as _;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryEntry;
use rusqlite::Row;
use rusqlite::Transaction;
use rusqlite::types::Type;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;

use crate::error::SqliteError;
use crate::insert::Columns;
use crate::time;

#[derive(Debug)]
pub(crate) struct NewMessage<'a> {
    pub(crate) conversation_id: &'a str,
    pub(crate) agent: &'a str,
    pub(crate) channel: &'a str,
    pub(crate) role: Role,
    pub(crate) content: &'a str,
    pub(crate) tool_name: Option<&'a str>,
    pub(crate) tool_args: Option<&'a str>,
    pub(crate) tool_result: Option<&'a str>,
    pub(crate) metadata: Option<&'a str>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) owner: &'a str,
}

impl NewMessage<'_> {
    pub(crate) fn insert(&self, tx: &Transaction<'_>) -> Result<String, SqliteError> {
        self.insert_as(tx, Uuid::new_v4().to_string())
    }

    pub(crate) fn insert_as(
        &self,
        tx: &Transaction<'_>,
        id: String,
    ) -> Result<String, SqliteError> {
        let role = self.role.to_string();
        let created_at = time::stamp(self.created_at);
        Columns::default()
            .set("id", &id)
            .set("conversation_id", &self.conversation_id)
            .set("agent", &self.agent)
            .set("channel", &self.channel)
            .set("role", &role)
            .set("content", &self.content)
            .set("tool_name", &self.tool_name)
            .set("tool_args", &self.tool_args)
            .set("tool_result", &self.tool_result)
            .set_supplied("metadata", self.metadata.as_ref())
            .set("created_at", &created_at)
            .set("owner_user_id", &self.owner)
            .insert(tx, "ros_messages", "message insert")?;
        Ok(id)
    }
}

pub(crate) const ENTRY_COLUMNS: &str = "m.id, c.session_key, m.agent, m.channel, m.role, m.content, \
     m.tool_name, m.tool_args, m.tool_result, m.metadata, m.created_at, \
     CASE WHEN c.session_key LIKE 'import:%' THEN COALESCE(c.settings, '{}') END";

#[derive(Debug)]
pub(crate) struct EntryRow {
    id: String,
    session_key: String,
    agent: String,
    channel: String,
    role: Role,
    content: String,
    tool_name: Option<String>,
    tool_args: Option<String>,
    tool_result: Option<String>,
    metadata: String,
    created_at: String,
    import_settings: Option<String>,
}

impl EntryRow {
    pub(crate) fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            session_key: row.get(1)?,
            agent: row.get(2)?,
            channel: row.get(3)?,
            role: stored_role(row, 4)?,
            content: row.get(5)?,
            tool_name: row.get(6)?,
            tool_args: row.get(7)?,
            tool_result: row.get(8)?,
            metadata: row.get(9)?,
            created_at: row.get(10)?,
            import_settings: row.get(11)?,
        })
    }
}

impl EntryRow {
    pub(crate) fn into_entry(
        mut self,
        seen: &mut std::collections::HashMap<String, Option<kernel::memory::Provenance>>,
    ) -> Result<MemoryEntry, SqliteError> {
        let settings = self.import_settings.take();
        let mut entry = MemoryEntry::try_from(self)?;
        if let Some(text) = settings {
            entry.provenance = match seen.get(&entry.session_id) {
                Some(known) => known.clone(),
                None => {
                    let parsed = Some(kernel::memory::Provenance::from_settings(&stored_object(
                        &text, "settings",
                    )?));
                    seen.insert(entry.session_id.clone(), parsed.clone());
                    parsed
                }
            };
        }
        Ok(entry)
    }
}

impl TryFrom<EntryRow> for MemoryEntry {
    type Error = SqliteError;

    fn try_from(row: EntryRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Some(row.id),
            session_id: row.session_key,
            agent: row.agent,
            channel: row.channel,
            role: row.role,
            content: row.content,
            tool_name: row.tool_name,
            tool_args: row
                .tool_args
                .as_deref()
                .map(|text| stored_object(text, "tool_args"))
                .transpose()?,
            tool_result: row.tool_result,
            metadata: Some(stored_object(&row.metadata, "metadata")?),
            created_at: Some(time::stored(&row.created_at, "created_at")?),
            provenance: None,
        })
    }
}

pub(crate) fn stored_role(row: &Row<'_>, index: usize) -> rusqlite::Result<Role> {
    let text = row.get_ref(index)?.as_str()?;
    Role::from_str(text).map_err(|source| {
        rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(source))
    })
}

fn stored_object(text: &str, column: &'static str) -> Result<JsonObject, SqliteError> {
    serde_json::from_str(text).map_err(|source| SqliteError::StoredJson { column, source })
}
