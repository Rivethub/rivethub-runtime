use std::collections::BTreeMap;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::DeletedSession;

use crate::PostgresMemory;
use crate::deletion::Erasure;
use crate::error::Error;

impl PostgresMemory {
    pub(crate) async fn keep(
        &self,
        session_key: &str,
        kept_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
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

    pub(crate) async fn keeps(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, Error> {
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

    pub(crate) async fn unkeep(&self, session_key: &str) -> Result<(), Error> {
        sqlx::query("DELETE FROM rh_session_keeps WHERE session_key = $1")
            .bind(session_key)
            .execute(&self.pool)
            .await
            .map_err(|source| Error::operation("session unkeep", source))?;
        Ok(())
    }

    pub(crate) async fn trash(
        &self,
        session_key: &str,
        trashed_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        sqlx::query(
            "INSERT INTO rh_session_trash (session_key, trashed_at, trashed_by) VALUES ($1, $2, $3)
             ON CONFLICT (session_key) DO NOTHING",
        )
        .bind(session_key)
        .bind(now)
        .bind(trashed_by)
        .execute(&self.pool)
        .await
        .map_err(|source| Error::operation("session trash", source))?;
        Ok(())
    }

    pub(crate) async fn restore(&self, session_key: &str) -> Result<(), Error> {
        sqlx::query("DELETE FROM rh_session_trash WHERE session_key = $1")
            .bind(session_key)
            .execute(&self.pool)
            .await
            .map_err(|source| Error::operation("session restore", source))?;
        Ok(())
    }

    pub(crate) async fn trash_marks(
        &self,
        session_keys: &[String],
    ) -> Result<BTreeMap<String, DateTime<Utc>>, Error> {
        let found: Vec<(String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT session_key, trashed_at FROM rh_session_trash WHERE session_key = ANY($1)",
        )
        .bind(session_keys)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("session trash marks", source))?;
        Ok(found.into_iter().collect())
    }

    pub(crate) async fn purge_trash(
        &self,
        older_than: DateTime<Utc>,
        limit: u32,
    ) -> Result<DeletedSession, Error> {
        let due: Vec<String> = sqlx::query_scalar(
            "SELECT session_key FROM rh_session_trash WHERE trashed_at <= $1
              ORDER BY trashed_at, session_key LIMIT $2",
        )
        .bind(older_than)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("trash sweep", source))?;
        let mut total = DeletedSession::default();
        for session_key in due {
            total += self
                .erase(&session_key, Erasure::Trashed { older_than })
                .await?;
        }
        Ok(total)
    }
}

pub(crate) fn not_trashed(alias: &str) -> String {
    format!(
        " AND NOT EXISTS (SELECT 1 FROM rh_conversations trashed_conv \
         JOIN rh_session_trash trashed ON trashed.session_key = trashed_conv.session_key \
         WHERE trashed_conv.id = {alias}.conversation_id)"
    )
}
