use chrono::DateTime;
use chrono::Utc;
use rusqlite::OptionalExtension as _;
use rusqlite::Transaction;
use rusqlite::params;
use uuid::Uuid;

use crate::error::SqliteError;
use crate::insert::Columns;
use crate::time;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationKey {
    pub(crate) session_key: String,
    pub(crate) agent: String,
}

#[derive(Debug)]
pub(crate) struct StoredConversation {
    pub(crate) id: String,
    pub(crate) channel: String,
    pub(crate) updated_at: DateTime<Utc>,
    pub(crate) owner: Option<String>,
}

impl ConversationKey {
    pub(crate) fn find(
        &self,
        tx: &Transaction<'_>,
    ) -> Result<Option<StoredConversation>, SqliteError> {
        let found = tx
            .query_row(
                "SELECT id, channel, updated_at, owner_user_id FROM ros_conversations
                  WHERE session_key = ?1 AND agent = ?2",
                params![self.session_key, self.agent],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(SqliteError::query("conversation lookup"))?;
        found
            .map(|(id, channel, updated_at, owner)| {
                Ok(StoredConversation {
                    id,
                    channel,
                    updated_at: time::stored(&updated_at, "updated_at")?,
                    owner,
                })
            })
            .transpose()
    }
}

#[derive(Debug)]
pub(crate) struct NewConversation<'a> {
    pub(crate) key: &'a ConversationKey,
    pub(crate) channel: Option<&'a str>,
    pub(crate) title: Option<&'a str>,
    pub(crate) settings: Option<&'a str>,
    pub(crate) task_id: Option<&'a str>,
    pub(crate) owner: &'a str,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

impl NewConversation<'_> {
    pub(crate) fn insert(&self, tx: &Transaction<'_>) -> Result<StoredConversation, SqliteError> {
        let id = Uuid::new_v4().to_string();
        let created_at = time::stamp(self.created_at);
        let updated_at = time::stamp(self.updated_at);
        Columns::default()
            .set("id", &id)
            .set("session_key", &self.key.session_key)
            .set("agent", &self.key.agent)
            .set_supplied("channel", self.channel.as_ref())
            .set("title", &self.title)
            .set_supplied("settings", self.settings.as_ref())
            .set("task_id", &self.task_id)
            .set("created_at", &created_at)
            .set("updated_at", &updated_at)
            .set("owner_user_id", &self.owner)
            .insert(tx, "ros_conversations", "conversation insert")?;
        let channel = tx
            .query_row(
                "SELECT channel FROM ros_conversations WHERE id = ?1",
                [&id],
                |row| row.get(0),
            )
            .map_err(SqliteError::query("conversation insert"))?;
        Ok(StoredConversation {
            id,
            channel,
            updated_at: self.updated_at,
            owner: Some(self.owner.to_owned()),
        })
    }
}

pub(crate) fn origins(
    conn: &rusqlite::Connection,
    session_keys: &[String],
) -> Result<std::collections::BTreeMap<String, kernel::memory::Provenance>, SqliteError> {
    let wanted: Vec<&String> = session_keys
        .iter()
        .filter(|key| kernel::memory::is_imported(key))
        .collect();
    if wanted.is_empty() {
        return Ok(std::collections::BTreeMap::new());
    }
    let marks = vec!["?"; wanted.len()].join(", ");
    let mut statement = conn
        .prepare(&format!(
            "SELECT session_key, settings FROM ros_conversations WHERE session_key IN ({marks})"
        ))
        .map_err(SqliteError::query("origin lookup"))?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(wanted), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(SqliteError::query("origin lookup"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("origin lookup"))?;
    rows.into_iter()
        .map(|(session_key, settings)| {
            let settings = match settings {
                Some(text) => {
                    serde_json::from_str(&text).map_err(|source| SqliteError::StoredJson {
                        column: "settings",
                        source,
                    })?
                }
                None => wire::JsonObject::default(),
            };
            Ok((
                session_key,
                kernel::memory::Provenance::from_settings(&settings),
            ))
        })
        .collect()
}
