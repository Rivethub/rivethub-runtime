use kernel::memory::{
    SummaryStoreError,
    summary_backend::{SummaryIndex, SummaryRepair},
    summary_reference::SummarySectionBounds,
};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{PostgresMemory, summary_backend::error, summary_replace::exclusive};

impl PostgresMemory {
    pub(crate) async fn marked_indexes(&self, limit: u32) -> Result<Vec<Uuid>, SummaryStoreError> {
        let Some(key) = self.summary_index_key else {
            return Ok(Vec::new());
        };
        sqlx::query_scalar(
            "SELECT s.id FROM rh_summaries s
             WHERE s.kind IN ('session', 'part')
               AND (s.index_needs_repair OR s.index_key IS DISTINCT FROM $1)
               AND NOT EXISTS (SELECT 1 FROM graphile_worker._private_jobs j WHERE j.key = 'embed-ros_summaries-' || s.id)
               AND NOT EXISTS (SELECT 1 FROM rh_flagged_jobs j WHERE j.job_key = 'embed-ros_summaries-' || s.id)
             ORDER BY s.id LIMIT $2"
        ).bind(key.as_slice()).bind(i64::from(limit)).fetch_all(&self.pool).await.map_err(error)
    }

    pub(crate) async fn marked_index(
        &self,
        id: Uuid,
    ) -> Result<Option<SummaryRepair>, SummaryStoreError> {
        let Some(key) = self.summary_index_key else {
            return Ok(None);
        };
        let mut tx = self.pool.begin().await.map_err(error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(error)?;
        let row: Option<(String, Vec<u8>)> = sqlx::query_as(
            "SELECT content, input_key FROM rh_summaries WHERE id = $1 AND kind IN ('session', 'part')
               AND (index_needs_repair OR index_key IS DISTINCT FROM $2)"
        ).bind(id).bind(key.as_slice()).fetch_optional(&mut *tx).await.map_err(error)?;
        let Some((content, input_key)) = row else {
            return Ok(None);
        };
        let sources: Vec<(i32, jiff_sqlx::Timestamp)> = sqlx::query_as(
            "SELECT l.ordinal, m.created_at FROM rh_summary_sources l
             JOIN rh_messages m ON m.id = l.message_id WHERE l.summary_id = $1 ORDER BY l.ordinal",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(error)?;
        let mut sections = Vec::new();
        let rows: Vec<(i32, i32, bool, i32, i32)> = sqlx::query_as(
            "SELECT source_first, source_last, range_verified, section_char_start, section_char_end
             FROM rh_summaries WHERE parent_id = $1 AND kind = 'section' ORDER BY summary_ordinal",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(error)?;
        for (first, last, verified, char_start, char_end) in rows {
            sections.push(SummarySectionBounds {
                first,
                last,
                verified,
                char_start,
                char_end,
            });
        }
        tx.commit().await.map_err(error)?;
        Ok(Some(SummaryRepair {
            id,
            content,
            input_key,
            sources: sources
                .into_iter()
                .map(|(ordinal, at)| (ordinal, at.to_jiff()))
                .collect(),
            sections,
        }))
    }

    pub(crate) async fn replace_index(
        &self,
        expected: SummaryRepair,
        index: SummaryIndex,
    ) -> Result<(), SummaryStoreError> {
        let Some(key) = self.summary_index_key else {
            return Ok(());
        };
        index.validate_width().map_err(error)?;
        let mut tx = self.pool.begin().await.map_err(error)?;
        let conversation: Option<Uuid> = sqlx::query_scalar(
            "SELECT conversation_id FROM rh_summaries WHERE id = $1 AND kind IN ('session', 'part')"
        ).bind(expected.id).fetch_optional(&mut *tx).await.map_err(error)?;
        let Some(conversation) = conversation else {
            return Ok(());
        };
        exclusive(&mut tx, [conversation]).await.map_err(error)?;
        let updated = sqlx::query(
            "UPDATE rh_summaries SET index_key = $4 WHERE id = $1 AND input_key = $2 AND content = $3
               AND (index_needs_repair OR index_key IS DISTINCT FROM $4)"
        ).bind(expected.id).bind(expected.input_key).bind(expected.content).bind(key.as_slice())
            .execute(&mut *tx).await.map_err(error)?;
        if updated.rows_affected() == 0 {
            return Ok(());
        }
        sqlx::query("DELETE FROM rh_summary_passages WHERE record_id = $1")
            .bind(expected.id)
            .execute(&mut *tx)
            .await
            .map_err(error)?;
        insert_passages(&mut tx, expected.id, key, index).await?;
        let complete = sqlx::query(
            "UPDATE rh_summaries SET index_needs_repair = false
             WHERE id = $1 AND rh_summary_record_indexed(id)",
        )
        .bind(expected.id)
        .execute(&mut *tx)
        .await
        .map_err(error)?;
        if complete.rows_affected() != 1 {
            return Err(error(std::io::Error::other(
                "incomplete repaired summary index",
            )));
        }
        tx.commit().await.map_err(error)
    }
}

async fn insert_passages(
    conn: &mut PgConnection,
    id: Uuid,
    key: [u8; 32],
    index: SummaryIndex,
) -> Result<(), SummaryStoreError> {
    for (ordinal, piece) in index.passages.into_iter().enumerate() {
        sqlx::query(
            "INSERT INTO rh_summary_passages (id, record_id, ordinal, char_start, char_end, content,
                 earliest_at, latest_at, index_key, embedding)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
        ).bind(Uuid::new_v4()).bind(id).bind(i32::try_from(ordinal).map_err(error)?)
            .bind(piece.chars.start).bind(piece.chars.end).bind(piece.content)
            .bind(crate::time::BindTime::bind_time(piece.earliest_at)).bind(crate::time::BindTime::bind_time(piece.latest_at)).bind(key.as_slice())
            .bind(pgvector::HalfVector::from_f32_slice(&piece.vector))
            .execute(&mut *conn).await.map_err(error)?;
    }
    Ok(())
}
