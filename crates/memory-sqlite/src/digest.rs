use kernel::memory::SessionDigest;
use kernel::owner::Owner;
use rusqlite::Connection;
use uuid::Uuid;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

const DIGESTS: &str = "SELECT c.session_key, c.id, c.title, c.agent, \
     CASE WHEN c.session_key LIKE 'import:%' \
     THEN json_extract(c.settings, '$.origin.author') END, m.created_at \
     FROM ros_messages m \
     JOIN ros_conversations c ON c.id = m.conversation_id \
     WHERE (c.owner_user_id IS NULL OR c.owner_user_id = ?2) \
       AND c.session_key IN (SELECT value FROM json_each(?1)) \
       AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr WHERE tr.session_key = c.session_key) \
     ORDER BY c.session_key, c.id";

struct DigestRow {
    key: String,
    conversation_id: String,
    title: Option<String>,
    agent: String,
    imported_from: Option<String>,
    at: String,
}

impl SqliteStore {
    pub async fn session_digests(
        &self,
        owner: &Owner,
        keys: Vec<String>,
    ) -> Result<Vec<SessionDigest>, SqliteError> {
        let owner = owner.as_str().to_owned();
        self.with_conn(move |conn| read_digests(conn, &owner, &keys))
            .await
    }
}

fn read_digests(
    conn: &Connection,
    owner: &str,
    keys: &[String],
) -> Result<Vec<SessionDigest>, SqliteError> {
    let wanted = serde_json::to_string(keys).map_err(|source| SqliteError::StoredJson {
        column: "session keys",
        source,
    })?;
    let mut statement = conn
        .prepare(DIGESTS)
        .map_err(SqliteError::query("session digest"))?;
    let rows = statement
        .query_map(rusqlite::params![wanted, owner], |row| {
            Ok(DigestRow {
                key: row.get(0)?,
                conversation_id: row.get(1)?,
                title: row.get(2)?,
                agent: row.get(3)?,
                imported_from: row.get(4)?,
                at: row.get(5)?,
            })
        })
        .map_err(SqliteError::query("session digest"))?;
    let mut digests: Vec<SessionDigest> = Vec::new();
    for row in rows {
        let row = row.map_err(SqliteError::query("session digest"))?;
        let at = time::stored(&row.at, "created_at")?;
        let conversation_id =
            Uuid::parse_str(&row.conversation_id).map_err(|_| SqliteError::Invalid {
                message: format!("conversation id {} is not a UUID", row.conversation_id),
            })?;
        match digests.last_mut() {
            Some(digest) if digest.conversation_id == conversation_id => {
                digest.first = digest.first.min(at);
                digest.last = digest.last.max(at);
                digest.messages += 1;
            }
            Some(_) | None => digests.push(SessionDigest {
                key: row.key,
                conversation_id,
                title: row.title.filter(|title| !title.is_empty()),
                agent: row.agent,
                first: at,
                last: at,
                messages: 1,
                imported_from: row.imported_from,
            }),
        }
    }
    Ok(digests)
}
