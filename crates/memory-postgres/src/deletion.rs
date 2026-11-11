use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::DeletedSession;
use kernel::memory::MemoryStoreError;
use kernel::memory::SessionActivity;
use kernel::memory::SessionDeletion;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::PostgresMemory;
use crate::error::Error;

const TRASH_TO_TOMBSTONE: &str = "WITH taken AS (
    DELETE FROM rh_session_trash WHERE session_key = $1 AND trashed_at <= $2
    RETURNING session_key, trashed_at, trashed_by, owner_user_id
)
INSERT INTO rh_session_tombstones (session_key, deleted_at, deleted_by, owner_user_id)
SELECT session_key, trashed_at, trashed_by, owner_user_id FROM taken
ON CONFLICT (session_key) DO UPDATE
   SET deleted_at = EXCLUDED.deleted_at, deleted_by = EXCLUDED.deleted_by
RETURNING owner_user_id";

pub(crate) enum Erasure<'a> {
    Sweep,
    Delete {
        at: DateTime<Utc>,
        by: Option<&'a str>,
    },
    Trashed {
        older_than: DateTime<Utc>,
    },
}

pub(crate) fn count(value: i64) -> Result<u64, Error> {
    u64::try_from(value).map_err(Error::from)
}

pub(crate) struct Tombstone {
    pub(crate) deleted_at: DateTime<Utc>,
    pub(crate) owner: Option<String>,
}

pub(crate) async fn tombstone_at(
    conn: &mut PgConnection,
    session_key: &str,
) -> Result<Option<Tombstone>, Error> {
    sqlx::query!(
        "SELECT deleted_at, owner_user_id FROM rh_session_tombstones WHERE session_key = $1",
        session_key
    )
    .fetch_optional(conn)
    .await
    .map(|row| {
        row.map(|row| Tombstone {
            deleted_at: row.deleted_at,
            owner: row.owner_user_id,
        })
    })
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

async fn forget_session(
    conn: &mut PgConnection,
    session_key: &str,
    owner: Option<&str>,
) -> Result<(), Error> {
    sqlx::query(
        "WITH placements AS (DELETE FROM rh_session_placements
                              WHERE session_id = $1 AND ($2::text IS NULL OR owner_user_id = $2)),
              suggestions AS (DELETE FROM rh_session_suggestions
                               WHERE session_id = $1 AND ($2::text IS NULL OR owner_user_id = $2)),
              startup AS (DELETE FROM rh_session_startup
                           WHERE session_id = $1 AND ($2::text IS NULL OR owner_user_id = $2)),
              history AS (DELETE FROM rh_session_history
                           WHERE session_id = $1 AND ($2::text IS NULL OR owner_user_id = $2)),
              trash AS (DELETE FROM rh_session_trash
                         WHERE session_key = $1 AND ($2::text IS NULL OR owner_user_id = $2)),
              model_calls AS (DELETE FROM rh_model_calls
                               WHERE session_key = $1 AND ($2::text IS NULL OR owner_user_id = $2))
         DELETE FROM rh_filing_corrections WHERE session_id = $1 AND ($2::text IS NULL OR owner_user_id = $2)",
    )
    .bind(session_key)
    .bind(owner)
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

async fn purge(
    conn: &mut PgConnection,
    session_key: &str,
    owner: Option<&str>,
) -> Result<DeletedSession, Error> {
    let fail = |operation| move |source| Error::operation(operation, source);
    let conversations: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT id FROM rh_conversations
          WHERE session_key = $1 AND ($2::text IS NULL OR owner_user_id = $2) FOR UPDATE",
        session_key,
        owner
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
    forget_session(conn, session_key, owner).await?;
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
    pub(crate) async fn erase(
        &self,
        session_key: &str,
        erasure: Erasure<'_>,
    ) -> Result<DeletedSession, Error> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|source| Error::operation("session delete begin", source))?;
        lock(&mut tx, session_key).await?;
        let owner = match erasure {
            Erasure::Sweep => {
                let Some(found) = tombstone_at(&mut tx, session_key).await? else {
                    return Ok(DeletedSession::default());
                };
                if used_after(&mut tx, session_key, found.deleted_at).await? {
                    lift(&mut tx, session_key).await?;
                    tx.commit()
                        .await
                        .map_err(|source| Error::operation("tombstone lift commit", source))?;
                    return Ok(DeletedSession::default());
                }
                found.owner
            }
            Erasure::Delete {
                at: deleted_at,
                by: deleted_by,
            } => sqlx::query_scalar!(
                "INSERT INTO rh_session_tombstones (session_key, deleted_at, deleted_by)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (session_key) DO UPDATE
                    SET deleted_at = EXCLUDED.deleted_at, deleted_by = EXCLUDED.deleted_by
                 RETURNING owner_user_id",
                session_key,
                deleted_at,
                deleted_by
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(|source| Error::operation("tombstone write", source))?,
            Erasure::Trashed { older_than } => {
                let Some(owner) = sqlx::query_scalar::<_, Option<String>>(TRASH_TO_TOMBSTONE)
                    .bind(session_key)
                    .bind(older_than)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|source| Error::operation("trash purge tombstone", source))?
                else {
                    return Ok(DeletedSession::default());
                };
                owner
            }
        };
        let deleted = purge(&mut tx, session_key, owner.as_deref()).await?;
        tx.commit()
            .await
            .map_err(|source| Error::operation("session delete commit", source))?;
        Ok(deleted)
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
              WHERE EXISTS (SELECT 1 FROM rh_conversations c WHERE c.session_key = t.session_key
                             AND (t.owner_user_id IS NULL OR c.owner_user_id = t.owner_user_id))
                 OR EXISTS (SELECT 1 FROM rh_session_placements p WHERE p.session_id = t.session_key
                             AND (t.owner_user_id IS NULL OR p.owner_user_id = t.owner_user_id))
                 OR EXISTS (SELECT 1 FROM rh_session_suggestions s WHERE s.session_id = t.session_key
                             AND (t.owner_user_id IS NULL OR s.owner_user_id = t.owner_user_id))
                 OR EXISTS (SELECT 1 FROM rh_session_startup u WHERE u.session_id = t.session_key
                             AND (t.owner_user_id IS NULL OR u.owner_user_id = t.owner_user_id))
              ORDER BY t.deleted_at, t.session_key
              LIMIT $1",
            i64::from(limit)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("tombstone sweep", source))?;
        let mut total = DeletedSession::default();
        for session_key in pending {
            total += self.erase(&session_key, Erasure::Sweep).await?;
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
        self.erase(
            session_key,
            Erasure::Delete {
                at: now,
                by: deleted_by,
            },
        )
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
        self.keep(session_key, kept_by, now)
            .await
            .map_err(Into::into)
    }

    async fn kept(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, MemoryStoreError> {
        self.keeps(session_keys).await.map_err(Into::into)
    }

    async fn unkeep_session(&self, session_key: &str) -> Result<(), MemoryStoreError> {
        self.unkeep(session_key).await.map_err(Into::into)
    }

    async fn trash_session(
        &self,
        session_key: &str,
        trashed_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), MemoryStoreError> {
        self.trash(session_key, trashed_by, now)
            .await
            .map_err(Into::into)
    }

    async fn restore_session(&self, session_key: &str) -> Result<(), MemoryStoreError> {
        self.restore(session_key).await.map_err(Into::into)
    }

    async fn trashed(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, MemoryStoreError> {
        self.trash_marks(session_keys).await.map_err(Into::into)
    }

    async fn purge_trashed(
        &self,
        older_than: DateTime<Utc>,
        limit: u32,
    ) -> Result<DeletedSession, MemoryStoreError> {
        self.purge_trash(older_than, limit)
            .await
            .map_err(Into::into)
    }
}
