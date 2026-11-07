use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::DeletedSession;
use kernel::memory::MemoryStoreError;
use kernel::memory::SessionActivity;
use kernel::memory::SessionDeletion;
use kernel::memory::TRASH_MESSAGES_MAX;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::session_marks::{due_trash, kept_at, trashed_at};
use crate::time;

const SESSION_CONVERSATIONS: &str = "SELECT id FROM ros_conversations WHERE session_key = ?1";
const SESSION_SUMMARIES: &str = "SELECT id FROM ros_summaries WHERE conversation_id IN (SELECT id FROM ros_conversations WHERE session_key = ?1)";

fn count(value: i64) -> Result<u64, SqliteError> {
    u64::try_from(value).map_err(SqliteError::Count)
}

fn usize_count(value: usize) -> Result<u64, SqliteError> {
    u64::try_from(value).map_err(SqliteError::Count)
}

pub(crate) fn tombstone_at(
    conn: &Connection,
    session_key: &str,
) -> Result<Option<DateTime<Utc>>, SqliteError> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT deleted_at FROM rh_session_tombstones WHERE session_key = ?1",
            [session_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(SqliteError::query("tombstone lookup"))?;
    stored
        .map(|text| time::stored(&text, "rh_session_tombstones.deleted_at"))
        .transpose()
}

pub(crate) fn lift(conn: &Connection, session_key: &str) -> Result<(), SqliteError> {
    conn.execute(
        "DELETE FROM rh_session_tombstones WHERE session_key = ?1",
        [session_key],
    )
    .map_err(SqliteError::query("tombstone lift"))?;
    tracing::info!(
        session_key,
        "deleted session was used again; tombstone lifted"
    );
    Ok(())
}

pub(crate) fn admits(
    conn: &Connection,
    session_key: &str,
    stamps: impl IntoIterator<Item = Option<DateTime<Utc>>>,
) -> Result<bool, SqliteError> {
    let Some(deleted_at) = tombstone_at(conn, session_key)? else {
        return Ok(true);
    };
    if kernel::memory::used_since(deleted_at, stamps) {
        lift(conn, session_key)?;
        return Ok(true);
    }
    Ok(false)
}

fn used_after(
    conn: &Connection,
    session_key: &str,
    deleted_at: DateTime<Utc>,
) -> Result<bool, SqliteError> {
    conn.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM ros_messages m
               JOIN ros_conversations c ON c.id = m.conversation_id
              WHERE c.session_key = ?1 AND julianday(m.created_at) > julianday(?2)
         )",
        params![session_key, time::stamp(deleted_at)],
        |row| row.get(0),
    )
    .map_err(SqliteError::query("tombstone use check"))
}

fn purge(conn: &Connection, session_key: &str) -> Result<DeletedSession, SqliteError> {
    conn.execute(
        &format!(
            "DELETE FROM ros_tags
              WHERE (entity_type = 'conversation' AND entity_id IN ({SESSION_CONVERSATIONS}))
                 OR (entity_type = 'summary' AND entity_id IN ({SESSION_SUMMARIES}))"
        ),
        [session_key],
    )
    .map_err(SqliteError::query("session tags delete"))?;
    conn.execute(
        &format!(
            "DELETE FROM rh_wiki_deferred
              WHERE waits_session = ?1
                 OR waits_conversation IN ({SESSION_CONVERSATIONS})
                 OR summary_id IN ({SESSION_SUMMARIES})"
        ),
        [session_key],
    )
    .map_err(SqliteError::query("session deferred notes delete"))?;
    conn.execute(
        &format!("DELETE FROM rh_wiki_entity_candidates WHERE summary_id IN ({SESSION_SUMMARIES})"),
        [session_key],
    )
    .map_err(SqliteError::query("session entity candidates delete"))?;
    let messages: i64 = conn
        .query_row(
            "SELECT count(*) FROM ros_messages
              WHERE conversation_id IN (SELECT id FROM ros_conversations WHERE session_key = ?1)",
            [session_key],
            |row| row.get(0),
        )
        .map_err(SqliteError::query("session messages count"))?;
    let summaries = conn
        .execute(
            &format!("DELETE FROM ros_summaries WHERE id IN ({SESSION_SUMMARIES})"),
            [session_key],
        )
        .map_err(SqliteError::query("session summaries delete"))?;
    for (table, operation) in [
        ("rh_session_placements", "session placement delete"),
        ("rh_session_suggestions", "session suggestion delete"),
        ("rh_session_startup", "session startup delete"),
        ("rh_session_history", "session history delete"),
        ("rh_filing_corrections", "session filing corrections delete"),
    ] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE session_id = ?1"),
            [session_key],
        )
        .map_err(SqliteError::query(operation))?;
    }
    conn.execute(
        "DELETE FROM rh_session_trash WHERE session_key = ?1",
        [session_key],
    )
    .map_err(SqliteError::query("session trash delete"))?;
    let conversations = conn
        .execute(
            "DELETE FROM ros_conversations WHERE session_key = ?1",
            [session_key],
        )
        .map_err(SqliteError::query("session conversations delete"))?;
    Ok(DeletedSession {
        conversations: usize_count(conversations)?,
        messages: count(messages)?,
        summaries: usize_count(summaries)?,
    })
}

fn erase(
    conn: &mut Connection,
    session_key: &str,
    tombstone: Option<(DateTime<Utc>, Option<&str>)>,
) -> Result<DeletedSession, SqliteError> {
    let tx = conn
        .transaction()
        .map_err(SqliteError::query("session delete begin"))?;
    if tombstone.is_none() {
        let Some(deleted_at) = tombstone_at(&tx, session_key)? else {
            return Ok(DeletedSession::default());
        };
        if used_after(&tx, session_key, deleted_at)? {
            lift(&tx, session_key)?;
            tx.commit()
                .map_err(SqliteError::query("tombstone lift commit"))?;
            return Ok(DeletedSession::default());
        }
    }
    if let Some((deleted_at, deleted_by)) = tombstone {
        tx.execute(
            "INSERT INTO rh_session_tombstones (session_key, deleted_at, deleted_by)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (session_key) DO UPDATE
                SET deleted_at = excluded.deleted_at, deleted_by = excluded.deleted_by",
            params![session_key, time::stamp(deleted_at), deleted_by],
        )
        .map_err(SqliteError::query("tombstone write"))?;
    }
    let deleted = purge(&tx, session_key)?;
    tx.commit()
        .map_err(SqliteError::query("session delete commit"))?;
    Ok(deleted)
}

fn found_keys(conn: &Connection, sql: &str, keys: &str) -> Result<Vec<String>, SqliteError> {
    let mut statement = conn
        .prepare(sql)
        .map_err(SqliteError::query("session keys"))?;
    let rows = statement
        .query_map([keys], |row| row.get::<_, String>(0))
        .map_err(SqliteError::query("session keys"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("session keys"))
}

struct ActivityRow {
    session_key: String,
    conversations: i64,
    messages: i64,
    user_messages: i64,
    assistant_messages: i64,
    tool_rows: i64,
    attachments: i64,
    tasked: bool,
    reply: Option<String>,
}

const ACTIVITY: &str = "
WITH conv AS (
    SELECT c.id, c.session_key, c.task_id,
           (SELECT count(*) FROM (
                SELECT 1 FROM ros_messages m WHERE m.conversation_id = c.id LIMIT ?2 + 1
            )) AS probed
      FROM ros_conversations c
     WHERE c.session_key IN (SELECT value FROM json_each(?1))
), sessions AS (
    SELECT session_key, count(*) AS conversations, sum(probed) AS messages,
           max(task_id IS NOT NULL) AS tasked
      FROM conv
     GROUP BY session_key
), talk AS (
    SELECT conv.session_key,
           count(*) FILTER (WHERE m.role = 'user' AND trim(m.content, ' ' || char(9, 10, 13)) <> '') AS user_messages,
           count(*) FILTER (WHERE m.role = 'assistant') AS assistant_messages,
           count(*) FILTER (
               WHERE m.role = 'tool' OR m.tool_name IS NOT NULL
                  OR m.tool_args IS NOT NULL OR m.tool_result IS NOT NULL
           ) AS tool_rows,
           count(*) FILTER (
               WHERE json_type(m.metadata, '$.attachments') IS NOT NULL
                  OR json_type(m.metadata, '$.images') IS NOT NULL
           ) AS attachments,
           (SELECT r.content FROM ros_messages r
              JOIN ros_conversations rc ON rc.id = r.conversation_id
             WHERE rc.session_key = conv.session_key AND r.role = 'assistant'
             ORDER BY r.created_at, r.id LIMIT 1) AS reply
      FROM conv
      JOIN sessions ON sessions.session_key = conv.session_key
      JOIN ros_messages m ON m.conversation_id = conv.id
     WHERE sessions.messages <= ?2 AND NOT sessions.tasked
     GROUP BY conv.session_key
)
SELECT s.session_key, s.conversations, s.messages,
       coalesce(t.user_messages, 0), coalesce(t.assistant_messages, 0),
       coalesce(t.tool_rows, 0), coalesce(t.attachments, 0), s.tasked, t.reply
  FROM sessions s
  LEFT JOIN talk t ON t.session_key = s.session_key";

const CITED: &str = "
WITH conv AS (
    SELECT id, session_key FROM ros_conversations
     WHERE session_key IN (SELECT value FROM json_each(?1))
), sums AS (
    SELECT s.id, conv.session_key FROM ros_summaries s JOIN conv ON conv.id = s.conversation_id
)
SELECT sums.session_key FROM sums
 WHERE EXISTS (SELECT 1 FROM ros_wiki_citations w WHERE w.summary_id = sums.id)
    OR EXISTS (
           SELECT 1 FROM ros_wiki_provenance p
            WHERE p.source_kind = 'summary' AND p.source_id = sums.id
       )
UNION
SELECT conv.session_key FROM conv
 WHERE EXISTS (SELECT 1 FROM ros_wiki_provenance p WHERE p.conversation_id = conv.id)
    OR EXISTS (
           SELECT 1 FROM ros_wiki_provenance p
            WHERE p.source_kind = 'conversation' AND p.source_id = conv.id
       )
UNION
SELECT conv.session_key
  FROM ros_wiki_provenance p
  JOIN ros_messages m ON m.id = p.source_id
  JOIN conv ON conv.id = m.conversation_id
 WHERE p.source_kind = 'message'";

const PLACED: &str = "
SELECT session_id FROM rh_session_placements
 WHERE session_id IN (SELECT value FROM json_each(?1))
UNION
SELECT c.session_key FROM rh_conversation_placements p
  JOIN ros_conversations c ON c.id = p.conversation_id
 WHERE c.session_key IN (SELECT value FROM json_each(?1))";

fn activity_rows(conn: &Connection, keys: &str) -> Result<Vec<ActivityRow>, SqliteError> {
    let mut statement = conn
        .prepare(ACTIVITY)
        .map_err(SqliteError::query("session activity"))?;
    let most = i64::try_from(TRASH_MESSAGES_MAX).map_err(SqliteError::Count)?;
    let rows = statement
        .query_map(params![keys, most], |row| {
            Ok(ActivityRow {
                session_key: row.get(0)?,
                conversations: row.get(1)?,
                messages: row.get(2)?,
                user_messages: row.get(3)?,
                assistant_messages: row.get(4)?,
                tool_rows: row.get(5)?,
                attachments: row.get(6)?,
                tasked: row.get(7)?,
                reply: row.get(8)?,
            })
        })
        .map_err(SqliteError::query("session activity"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("session activity"))
}

fn activity(
    conn: &Connection,
    sessions: &[String],
    keys: &str,
) -> Result<BTreeMap<String, SessionActivity>, SqliteError> {
    let mut activity: BTreeMap<String, SessionActivity> = sessions
        .iter()
        .map(|session| (session.clone(), SessionActivity::default()))
        .collect();
    for row in activity_rows(conn, keys)? {
        activity.insert(
            row.session_key,
            SessionActivity {
                conversations: count(row.conversations)?,
                messages: count(row.messages)?,
                user_messages: count(row.user_messages)?,
                assistant_messages: count(row.assistant_messages)?,
                reply: row.reply,
                tool_rows: count(row.tool_rows)?,
                attachments: count(row.attachments)?,
                tasked: row.tasked,
                placed: false,
                cited: false,
            },
        );
    }
    let open: Vec<String> = activity
        .iter()
        .filter(|(_, entry)| !entry.kept())
        .map(|(key, _)| key.clone())
        .collect();
    let open = json_keys(&open)?;
    let placed = found_keys(conn, PLACED, &open)?;
    let cited = found_keys(conn, CITED, &open)?;
    for (session, entry) in &mut activity {
        entry.placed = placed.contains(session);
        entry.cited = cited.contains(session);
    }
    Ok(activity)
}

fn json_keys(sessions: &[String]) -> Result<String, SqliteError> {
    serde_json::to_string(sessions).map_err(|source| SqliteError::Encode {
        column: "session keys",
        source,
    })
}

fn pending(conn: &Connection, limit: u32) -> Result<Vec<String>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT t.session_key FROM rh_session_tombstones t
              WHERE EXISTS (SELECT 1 FROM ros_conversations c WHERE c.session_key = t.session_key)
                 OR EXISTS (SELECT 1 FROM rh_session_placements p WHERE p.session_id = t.session_key)
                 OR EXISTS (SELECT 1 FROM rh_session_suggestions s WHERE s.session_id = t.session_key)
                 OR EXISTS (SELECT 1 FROM rh_session_startup u WHERE u.session_id = t.session_key)
              ORDER BY t.deleted_at, t.session_key
              LIMIT ?1",
        )
        .map_err(SqliteError::query("tombstone sweep"))?;
    let rows = statement
        .query_map([limit], |row| row.get::<_, String>(0))
        .map_err(SqliteError::query("tombstone sweep"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("tombstone sweep"))
}

fn empty(conn: &Connection, limit: u32) -> Result<Vec<String>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT c.session_key FROM ros_conversations c
              WHERE NOT EXISTS (SELECT 1 FROM ros_messages m WHERE m.conversation_id = c.id)
                AND NOT EXISTS (
                        SELECT 1 FROM rh_session_tombstones t WHERE t.session_key = c.session_key
                    )
              GROUP BY c.session_key
             HAVING count(*) = (
                        SELECT count(*) FROM ros_conversations o WHERE o.session_key = c.session_key
                    )
              ORDER BY max(julianday(c.updated_at)) DESC
              LIMIT ?1",
        )
        .map_err(SqliteError::query("empty sessions"))?;
    let rows = statement
        .query_map([limit], |row| row.get::<_, String>(0))
        .map_err(SqliteError::query("empty sessions"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("empty sessions"))
}

#[async_trait]
impl SessionDeletion for SqliteStore {
    async fn delete_session(
        &self,
        session_key: &str,
        deleted_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<DeletedSession, MemoryStoreError> {
        let session_key = session_key.to_owned();
        let deleted_by = deleted_by.map(str::to_owned);
        self.with_conn(move |conn| erase(conn, &session_key, Some((now, deleted_by.as_deref()))))
            .await
            .map_err(Into::into)
    }

    async fn tombstones(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, MemoryStoreError> {
        let session_keys = session_keys.to_vec();
        self.with_conn(move |conn| {
            let mut found = BTreeMap::new();
            for session_key in session_keys {
                if let Some(deleted_at) = tombstone_at(conn, &session_key)? {
                    found.insert(session_key, deleted_at);
                }
            }
            Ok(found)
        })
        .await
        .map_err(Into::into)
    }

    async fn session_activity(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, SessionActivity>, MemoryStoreError> {
        let keys = json_keys(session_keys)?;
        let sessions = session_keys.to_vec();
        self.with_conn(move |conn| activity(conn, &sessions, &keys))
            .await
            .map_err(Into::into)
    }

    async fn empty_sessions(&self, limit: u32) -> Result<Vec<String>, MemoryStoreError> {
        self.with_conn(move |conn| empty(conn, limit))
            .await
            .map_err(Into::into)
    }

    async fn purge_tombstoned(&self, limit: u32) -> Result<DeletedSession, MemoryStoreError> {
        self.with_conn(move |conn| {
            let mut total = DeletedSession::default();
            for session_key in pending(conn, limit)? {
                total += erase(conn, &session_key, None)?;
            }
            Ok(total)
        })
        .await
        .map_err(Into::into)
    }

    async fn keep_session(
        &self,
        session_key: &str,
        kept_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), MemoryStoreError> {
        let session_key = session_key.to_owned();
        let kept_by = kept_by.map(str::to_owned);
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO rh_session_keeps (session_key, kept_at, kept_by)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT (session_key) DO UPDATE
                    SET kept_at = excluded.kept_at, kept_by = excluded.kept_by",
                params![session_key, time::stamp(now), kept_by],
            )
            .map_err(SqliteError::query("session keep"))?;
            Ok(())
        })
        .await
        .map_err(Into::into)
    }

    async fn kept(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, MemoryStoreError> {
        let keys = json_keys(session_keys)?;
        self.with_conn(move |conn| kept_at(conn, &keys))
            .await
            .map_err(Into::into)
    }

    async fn unkeep_session(&self, session_key: &str) -> Result<(), MemoryStoreError> {
        let session_key = session_key.to_owned();
        self.with_conn(move |conn| {
            conn.execute(
                "DELETE FROM rh_session_keeps WHERE session_key = ?1",
                [session_key],
            )
            .map_err(SqliteError::query("session unkeep"))?;
            Ok(())
        })
        .await
        .map_err(Into::into)
    }

    async fn trash_session(
        &self,
        session_key: &str,
        trashed_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), MemoryStoreError> {
        let session_key = session_key.to_owned();
        let trashed_by = trashed_by.map(str::to_owned);
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO rh_session_trash (session_key, trashed_at, trashed_by)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT (session_key) DO NOTHING",
                params![session_key, time::stamp(now), trashed_by],
            )
            .map_err(SqliteError::query("session trash"))?;
            Ok(())
        })
        .await
        .map_err(Into::into)
    }

    async fn restore_session(&self, session_key: &str) -> Result<(), MemoryStoreError> {
        let session_key = session_key.to_owned();
        self.with_conn(move |conn| {
            conn.execute(
                "DELETE FROM rh_session_trash WHERE session_key = ?1",
                [session_key],
            )
            .map_err(SqliteError::query("session restore"))?;
            Ok(())
        })
        .await
        .map_err(Into::into)
    }

    async fn trashed(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, MemoryStoreError> {
        let keys = json_keys(session_keys)?;
        self.with_conn(move |conn| trashed_at(conn, &keys))
            .await
            .map_err(Into::into)
    }

    async fn purge_trashed(
        &self,
        older_than: DateTime<Utc>,
        limit: u32,
    ) -> Result<DeletedSession, MemoryStoreError> {
        self.with_conn(move |conn| {
            let now = Utc::now();
            let mut total = DeletedSession::default();
            for session_key in due_trash(conn, older_than, limit)? {
                total += erase(conn, &session_key, Some((now, None)))?;
            }
            Ok(total)
        })
        .await
        .map_err(Into::into)
    }
}
