use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::DeletedSession;
use kernel::memory::MemoryStoreError;
use kernel::memory::SessionActivity;
use kernel::memory::SessionDeletion;
use kernel::memory::TRASH_MESSAGES_MAX;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::PostgresMemory;
use crate::error::Error;

struct ActivityRow {
    session_key: String,
    conversations: i64,
    messages: i64,
    user_messages: i64,
    assistant_messages: i64,
    tool_rows: i64,
    attachments: i64,
    tasked: bool,
    reply: Option<Vec<u8>>,
    reply_bytes: Option<i32>,
}

const REPLY_BYTES: i32 = 512;

fn reply_text(bytes: Vec<u8>, whole: Option<i32>) -> Result<String, Error> {
    let cut =
        whole.is_some_and(|whole| usize::try_from(whole).is_ok_and(|whole| whole > bytes.len()));
    let mut text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let valid = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).map_err(|source| Error::stored("trash reply", source))?
        }
    };
    if cut {
        text.push_str(" …");
    }
    Ok(text)
}

fn count(value: i64) -> Result<u64, Error> {
    u64::try_from(value).map_err(Error::from)
}

pub(crate) async fn tombstone_at(
    conn: &mut PgConnection,
    session_key: &str,
) -> Result<Option<DateTime<Utc>>, Error> {
    sqlx::query_scalar!(
        "SELECT deleted_at FROM rh_session_tombstones WHERE session_key = $1",
        session_key
    )
    .fetch_optional(conn)
    .await
    .map_err(|source| Error::operation("tombstone lookup", source))
}

pub(crate) async fn lift(conn: &mut PgConnection, session_key: &str) -> Result<(), Error> {
    sqlx::query!(
        "DELETE FROM rh_session_tombstones WHERE session_key = $1",
        session_key
    )
    .execute(conn)
    .await
    .map_err(|source| Error::operation("tombstone lift", source))?;
    tracing::info!(
        session_key,
        "deleted session was used again; tombstone lifted"
    );
    Ok(())
}

async fn used_after(
    conn: &mut PgConnection,
    session_key: &str,
    deleted_at: DateTime<Utc>,
) -> Result<bool, Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM rh_messages m
                 JOIN rh_conversations c ON c.id = m.conversation_id
                WHERE c.session_key = $1 AND m.created_at > $2
           ) AS "used!""#,
        session_key,
        deleted_at
    )
    .fetch_one(conn)
    .await
    .map_err(|source| Error::operation("tombstone use check", source))
}

async fn lock(conn: &mut PgConnection, session_key: &str) -> Result<(), Error> {
    sqlx::query!(
        "select pg_advisory_xact_lock(hashtext($1)) is not null as locked",
        session_key
    )
    .fetch_one(conn)
    .await
    .map_err(|source| Error::operation("session delete lock", source))?;
    Ok(())
}

async fn forget_session(conn: &mut PgConnection, session_key: &str) -> Result<(), Error> {
    sqlx::query!(
        "WITH placements AS (DELETE FROM rh_session_placements WHERE session_id = $1),
              suggestions AS (DELETE FROM rh_session_suggestions WHERE session_id = $1),
              startup AS (DELETE FROM rh_session_startup WHERE session_id = $1),
              history AS (DELETE FROM rh_session_history WHERE session_id = $1)
         DELETE FROM rh_filing_corrections WHERE session_id = $1",
        session_key
    )
    .execute(conn)
    .await
    .map_err(|source| Error::operation("session state delete", source))?;
    Ok(())
}

async fn forget_wiki_holds(
    conn: &mut PgConnection,
    session_key: &str,
    conversations: &[Uuid],
    summaries: &[Uuid],
) -> Result<(), Error> {
    sqlx::query!(
        "WITH candidates AS (DELETE FROM rh_wiki_entity_candidates WHERE summary_id = ANY($3))
         DELETE FROM rh_wiki_deferred
          WHERE waits_session = $1 OR waits_conversation = ANY($2) OR summary_id = ANY($3)",
        session_key,
        conversations,
        summaries
    )
    .execute(conn)
    .await
    .map_err(|source| Error::operation("session wiki holds delete", source))?;
    Ok(())
}

async fn purge(conn: &mut PgConnection, session_key: &str) -> Result<DeletedSession, Error> {
    let fail = |operation| move |source| Error::operation(operation, source);
    let conversations: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT id FROM rh_conversations WHERE session_key = $1 FOR UPDATE",
        session_key
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(fail("session conversations"))?;
    let summaries: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT id FROM rh_summaries WHERE conversation_id = ANY($1)",
        &conversations
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(fail("session summaries"))?;
    sqlx::query!(
        "DELETE FROM rh_tags
          WHERE (entity_type = 'conversation' AND entity_id = ANY($1))
             OR (entity_type = 'summary' AND entity_id = ANY($2))",
        &conversations,
        &summaries
    )
    .execute(&mut *conn)
    .await
    .map_err(fail("session tags delete"))?;
    let messages = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM rh_messages WHERE conversation_id = ANY($1)"#,
        &conversations
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(fail("session messages count"))?;
    let removed_summaries = sqlx::query!("DELETE FROM rh_summaries WHERE id = ANY($1)", &summaries)
        .execute(&mut *conn)
        .await
        .map_err(fail("session summaries delete"))?
        .rows_affected();
    forget_session(conn, session_key).await?;
    forget_wiki_holds(conn, session_key, &conversations, &summaries).await?;
    let removed_conversations = sqlx::query!(
        "DELETE FROM rh_conversations WHERE id = ANY($1)",
        &conversations
    )
    .execute(&mut *conn)
    .await
    .map_err(fail("session conversations delete"))?
    .rows_affected();
    Ok(DeletedSession {
        conversations: removed_conversations,
        messages: count(messages)?,
        summaries: removed_summaries,
    })
}

impl PostgresMemory {
    async fn erase(
        &self,
        session_key: &str,
        tombstone: Option<(DateTime<Utc>, Option<&str>)>,
    ) -> Result<DeletedSession, Error> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|source| Error::operation("session delete begin", source))?;
        lock(&mut tx, session_key).await?;
        if tombstone.is_none() {
            let Some(deleted_at) = tombstone_at(&mut tx, session_key).await? else {
                return Ok(DeletedSession::default());
            };
            if used_after(&mut tx, session_key, deleted_at).await? {
                lift(&mut tx, session_key).await?;
                tx.commit()
                    .await
                    .map_err(|source| Error::operation("tombstone lift commit", source))?;
                return Ok(DeletedSession::default());
            }
        }
        if let Some((deleted_at, deleted_by)) = tombstone {
            sqlx::query!(
                "INSERT INTO rh_session_tombstones (session_key, deleted_at, deleted_by)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (session_key) DO UPDATE
                    SET deleted_at = EXCLUDED.deleted_at, deleted_by = EXCLUDED.deleted_by",
                session_key,
                deleted_at,
                deleted_by
            )
            .execute(&mut *tx)
            .await
            .map_err(|source| Error::operation("tombstone write", source))?;
        }
        let deleted = purge(&mut tx, session_key).await?;
        tx.commit()
            .await
            .map_err(|source| Error::operation("session delete commit", source))?;
        Ok(deleted)
    }

    async fn activity_rows(&self, keys: &[String]) -> Result<Vec<ActivityRow>, Error> {
        let most = i64::try_from(TRASH_MESSAGES_MAX)?;
        sqlx::query_as!(
            ActivityRow,
            r#"WITH conv AS (
                   SELECT c.id, c.session_key, c.task_id,
                          (SELECT count(*) FROM (
                               SELECT 1 FROM rh_messages m WHERE m.conversation_id = c.id LIMIT $2::bigint + 1
                           ) probed) AS probed
                     FROM rh_conversations c
                    WHERE c.session_key = ANY($1)
               ), sessions AS (
                   SELECT session_key,
                          count(*) AS conversations,
                          sum(probed)::bigint AS messages,
                          bool_or(task_id IS NOT NULL) AS tasked
                     FROM conv
                    GROUP BY session_key
               ), talk AS MATERIALIZED (
                   SELECT conv.session_key,
                          count(*) FILTER (
                              WHERE m.role = 'user' AND m.content ~ '[^[:space:]]'
                          ) AS user_messages,
                          count(*) FILTER (WHERE m.role = 'assistant') AS assistant_messages,
                          count(*) FILTER (
                              WHERE m.role = 'tool' OR m.tool_name IS NOT NULL
                                 OR m.tool_args IS NOT NULL OR m.tool_result IS NOT NULL
                          ) AS tool_rows,
                          count(*) FILTER (
                              WHERE m.metadata ? 'attachments' OR m.metadata ? 'images'
                          ) AS attachments,
                          (array_agg(substring(convert_to(m.content, 'UTF8') from 1 for $3)
                                     ORDER BY m.created_at, m.id)
                              FILTER (WHERE m.role = 'assistant'))[1] AS reply,
                          (array_agg(octet_length(m.content) ORDER BY m.created_at, m.id)
                              FILTER (WHERE m.role = 'assistant'))[1] AS reply_bytes
                     FROM conv
                     JOIN sessions ON sessions.session_key = conv.session_key
                     JOIN rh_messages m ON m.conversation_id = conv.id
                    WHERE sessions.messages <= $2::bigint AND NOT sessions.tasked
                    GROUP BY conv.session_key
               )
               SELECT s.session_key AS "session_key!",
                      s.conversations AS "conversations!",
                      s.messages AS "messages!",
                      coalesce(t.user_messages, 0) AS "user_messages!",
                      coalesce(t.assistant_messages, 0) AS "assistant_messages!",
                      coalesce(t.tool_rows, 0) AS "tool_rows!",
                      coalesce(t.attachments, 0) AS "attachments!",
                      s.tasked AS "tasked!",
                      t.reply,
                      t.reply_bytes
                 FROM sessions s
                 LEFT JOIN talk t ON t.session_key = s.session_key"#,
            keys,
            most,
            REPLY_BYTES
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("session activity", source))
    }

    async fn placed_keys(&self, keys: &[String]) -> Result<Vec<String>, Error> {
        sqlx::query_scalar!(
            r#"SELECT session_id AS "session_key!" FROM rh_session_placements
                WHERE session_id = ANY($1)
               UNION
               SELECT c.session_key FROM rh_conversation_placements p
                 JOIN rh_conversations c ON c.id = p.conversation_id
                WHERE c.session_key = ANY($1)"#,
            keys
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("session placements", source))
    }

    async fn cited_keys(&self, keys: &[String]) -> Result<Vec<String>, Error> {
        sqlx::query_scalar!(
            r#"WITH conv AS (
                   SELECT id, session_key FROM rh_conversations WHERE session_key = ANY($1)
               ), sums AS (
                   SELECT s.id, conv.session_key
                     FROM rh_summaries s
                     JOIN conv ON conv.id = s.conversation_id
               )
               SELECT sums.session_key AS "session_key!" FROM sums
                WHERE EXISTS (SELECT 1 FROM rh_wiki_citations w WHERE w.summary_id = sums.id)
                   OR EXISTS (
                          SELECT 1 FROM rh_wiki_provenance p
                           WHERE p.source_kind = 'summary' AND p.source_id = sums.id
                      )
               UNION
               SELECT conv.session_key FROM conv
                WHERE EXISTS (SELECT 1 FROM rh_wiki_provenance p WHERE p.conversation_id = conv.id)
                   OR EXISTS (
                          SELECT 1 FROM rh_wiki_provenance p
                           WHERE p.source_kind = 'conversation' AND p.source_id = conv.id
                      )
               UNION
               SELECT conv.session_key
                 FROM rh_wiki_provenance p
                 JOIN rh_messages m ON m.id = p.source_id
                 JOIN conv ON conv.id = m.conversation_id
                WHERE p.source_kind = 'message'"#,
            keys
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("session citations", source))
    }

    async fn activity(&self, keys: &[String]) -> Result<BTreeMap<String, SessionActivity>, Error> {
        let mut activity: BTreeMap<String, SessionActivity> = keys
            .iter()
            .map(|key| (key.clone(), SessionActivity::default()))
            .collect();
        for row in self.activity_rows(keys).await? {
            activity.insert(
                row.session_key,
                SessionActivity {
                    conversations: count(row.conversations)?,
                    messages: count(row.messages)?,
                    user_messages: count(row.user_messages)?,
                    assistant_messages: count(row.assistant_messages)?,
                    reply: row
                        .reply
                        .map(|reply| reply_text(reply, row.reply_bytes))
                        .transpose()?,
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
        let (placed, cited) = tokio::try_join!(self.placed_keys(&open), self.cited_keys(&open))?;
        for (key, entry) in &mut activity {
            entry.placed = placed.contains(key);
            entry.cited = cited.contains(key);
        }
        Ok(activity)
    }

    async fn empty_keys(&self, limit: u32) -> Result<Vec<String>, Error> {
        sqlx::query_scalar!(
            r#"SELECT c.session_key AS "session_key!"
                 FROM rh_conversations c
                WHERE NOT EXISTS (SELECT 1 FROM rh_messages m WHERE m.conversation_id = c.id)
                  AND NOT EXISTS (
                          SELECT 1 FROM rh_session_tombstones t WHERE t.session_key = c.session_key
                      )
                GROUP BY c.session_key
               HAVING count(*) = (
                          SELECT count(*) FROM rh_conversations o WHERE o.session_key = c.session_key
                      )
                ORDER BY max(c.updated_at) DESC
                LIMIT $1"#,
            i64::from(limit)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("empty sessions", source))
    }

    async fn purge_all(&self, limit: u32) -> Result<DeletedSession, Error> {
        let pending = sqlx::query_scalar!(
            "SELECT t.session_key FROM rh_session_tombstones t
              WHERE EXISTS (SELECT 1 FROM rh_conversations c WHERE c.session_key = t.session_key)
                 OR EXISTS (SELECT 1 FROM rh_session_placements p WHERE p.session_id = t.session_key)
                 OR EXISTS (SELECT 1 FROM rh_session_suggestions s WHERE s.session_id = t.session_key)
                 OR EXISTS (SELECT 1 FROM rh_session_startup u WHERE u.session_id = t.session_key)
              ORDER BY t.deleted_at, t.session_key
              LIMIT $1",
            i64::from(limit)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("tombstone sweep", source))?;
        let mut total = DeletedSession::default();
        for session_key in pending {
            total += self.erase(&session_key, None).await?;
        }
        Ok(total)
    }
}

#[async_trait]
impl SessionDeletion for PostgresMemory {
    async fn delete_session(
        &self,
        session_key: &str,
        deleted_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<DeletedSession, MemoryStoreError> {
        self.erase(session_key, Some((now, deleted_by)))
            .await
            .map_err(Into::into)
    }

    async fn tombstones(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, MemoryStoreError> {
        let found = sqlx::query!(
            "SELECT session_key, deleted_at FROM rh_session_tombstones WHERE session_key = ANY($1)",
            session_keys
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("tombstones", source))?;
        Ok(found
            .into_iter()
            .map(|row| (row.session_key, row.deleted_at))
            .collect())
    }

    async fn session_activity(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, SessionActivity>, MemoryStoreError> {
        self.activity(session_keys).await.map_err(Into::into)
    }

    async fn empty_sessions(&self, limit: u32) -> Result<Vec<String>, MemoryStoreError> {
        self.empty_keys(limit).await.map_err(Into::into)
    }

    async fn purge_tombstoned(&self, limit: u32) -> Result<DeletedSession, MemoryStoreError> {
        self.purge_all(limit).await.map_err(Into::into)
    }

    async fn keep_session(
        &self,
        session_key: &str,
        kept_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), MemoryStoreError> {
        sqlx::query!(
            "INSERT INTO rh_session_keeps (session_key, kept_at, kept_by) VALUES ($1, $2, $3)
             ON CONFLICT (session_key) DO UPDATE
                SET kept_at = EXCLUDED.kept_at, kept_by = EXCLUDED.kept_by",
            session_key,
            now,
            kept_by
        )
        .execute(&self.pool)
        .await
        .map_err(|source| Error::operation("session keep", source))?;
        Ok(())
    }

    async fn kept(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, MemoryStoreError> {
        let found = sqlx::query!(
            "SELECT session_key, kept_at FROM rh_session_keeps WHERE session_key = ANY($1)",
            session_keys
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("session keeps", source))?;
        Ok(found
            .into_iter()
            .map(|row| (row.session_key, row.kept_at))
            .collect())
    }
}
