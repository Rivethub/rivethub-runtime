use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryEntry;
use kernel::owner::Owner;
use rusqlite::Transaction;
use rusqlite::params;
use uuid::Uuid;

use crate::SqliteStore;
use crate::capture::encode;
use crate::conversation::ConversationKey;
use crate::conversation::NewConversation;
use crate::error::SqliteError;
use crate::message::NewMessage;
use crate::time;
use crate::time::Stamp;

const HEARTBEAT_PREFIX: &str = "heartbeat:";
const TASK_PREFIX: &str = "task:";

impl SqliteStore {
    pub async fn append_entries(
        &self,
        entries: Vec<MemoryEntry>,
        owner: &Owner,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>, SqliteError> {
        if let Some(entry) = entries
            .iter()
            .find(|entry| kernel::memory::is_imported(&entry.session_id))
        {
            return Err(SqliteError::Invalid {
                message: format!(
                    "{} is an imported session; only a receive writes one (CONTRACT note 150)",
                    entry.session_id
                ),
            });
        }
        let owner = owner.clone();
        self.with_conn(move |conn| {
            let tx = conn
                .transaction()
                .map_err(SqliteError::query("append begin"))?;
            let mut admitted = std::collections::BTreeMap::new();
            for entry in &entries {
                if admitted.contains_key(&entry.session_id) {
                    continue;
                }
                let stamps = entries
                    .iter()
                    .filter(|other| other.session_id == entry.session_id)
                    .map(|other| other.created_at);
                let admits = crate::deletion::admits(&tx, &entry.session_id, stamps)?;
                admitted.insert(entry.session_id.clone(), admits);
            }
            let mut ids = Vec::with_capacity(entries.len());
            for entry in &entries {
                if admitted.get(&entry.session_id) == Some(&false) {
                    tracing::debug!(
                        session_key = %entry.session_id,
                        "append dropped for a deleted session"
                    );
                    continue;
                }
                ids.push(append_one(&tx, entry, owner.as_str(), now)?);
            }
            tx.commit().map_err(SqliteError::query("append commit"))?;
            Ok(ids)
        })
        .await
    }
}

fn append_one(
    tx: &Transaction<'_>,
    entry: &MemoryEntry,
    owner: &str,
    now: DateTime<Utc>,
) -> Result<String, SqliteError> {
    let key = ConversationKey {
        session_key: entry.session_id.clone(),
        agent: entry.agent.clone(),
    };
    let task_id = task_id(entry);
    let conversation = match key.find(tx)? {
        Some(found) => {
            admit(tx, &found, owner)?;
            tx.execute(
                "UPDATE ros_conversations
                    SET updated_at = ?2,
                        task_id = coalesce(task_id, ?3),
                        owner_user_id = coalesce(owner_user_id, ?4)
                  WHERE id = ?1",
                params![
                    found.id,
                    time::stamp(now.max(found.updated_at)),
                    task_id,
                    owner
                ],
            )
            .map_err(SqliteError::query("conversation update"))?;
            found
        }
        None => NewConversation {
            key: &key,
            channel: Some(&entry.channel),
            title: Some(&title(entry)),
            settings: None,
            task_id: task_id.as_deref(),
            owner,
            created_at: now,
            updated_at: now,
        }
        .insert(tx)?,
    };
    let tool_args = entry
        .tool_args
        .as_ref()
        .map(|args| encode(args, "tool_args"))
        .transpose()?;
    let metadata = entry
        .metadata
        .as_ref()
        .map(|metadata| encode(metadata, "metadata"))
        .transpose()?;
    let message = NewMessage {
        conversation_id: &conversation.id,
        agent: &entry.agent,
        channel: &entry.channel,
        role: entry.role,
        content: &entry.content,
        tool_name: entry.tool_name.as_deref(),
        tool_args: tool_args.as_deref(),
        tool_result: entry.tool_result.as_deref(),
        metadata: metadata.as_deref(),
        created_at: Stamp::from(entry.created_at).at(now),
        owner: conversation.owner.as_deref().unwrap_or(owner),
    };
    match &entry.id {
        Some(id) => message.insert_as(tx, id.clone()),
        None => message.insert(tx),
    }
}

fn admit(
    tx: &Transaction<'_>,
    found: &crate::conversation::StoredConversation,
    owner: &str,
) -> Result<(), SqliteError> {
    if found.owner.as_deref().is_some_and(|stored| stored != owner) {
        return Err(SqliteError::Invalid {
            message: "this session belongs to another user".to_owned(),
        });
    }
    let received: bool = tx
        .query_row(
            "SELECT json_extract(settings, '$.receipt') IS NOT NULL FROM ros_conversations WHERE id = ?1",
            [&found.id],
            |row| row.get(0),
        )
        .map_err(SqliteError::query("conversation receipt"))?;
    if received {
        return Err(SqliteError::Invalid {
            message: "this session carries a receive receipt; only a receive writes it (CONTRACT note 150)"
                .to_owned(),
        });
    }
    Ok(())
}

fn title(entry: &MemoryEntry) -> String {
    if entry.session_id.starts_with(HEARTBEAT_PREFIX) {
        format!("Heartbeat {}", entry.agent)
    } else {
        format!("Session {}", entry.session_id)
    }
}

fn task_id(entry: &MemoryEntry) -> Option<String> {
    let from_key = entry.session_id.strip_prefix(TASK_PREFIX);
    let from_metadata = entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("taskId"))
        .and_then(|value| value.as_str());
    [from_key, from_metadata]
        .into_iter()
        .flatten()
        .find(|candidate| Uuid::parse_str(candidate).is_ok())
        .map(str::to_owned)
}
