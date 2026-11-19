use jiff::SignedDuration;
use jiff::Timestamp;
use kernel::memory::summary_backend::SummaryArtifact;
use kernel::memory::{
    SummaryStoreError,
    summary_backend::{SummaryIndexes, SummaryPermit},
    summary_conversion::{
        ConversionBucket, ConversionCharge, ConversionControl, ConversionLease, ConversionOutcome,
        ConversionProgress, ConversionUsage, GIANT_TOKENS, INVENTORY_PAGE_SIZE, LEASE,
        SummaryConversion,
    },
    summary_reference::PreparedSummaryTree,
};
use sqlx::{PgConnection, Row as _};
use uuid::Uuid;

use crate::{PostgresMemory, settlement::INCLUDED, summary_replace::ReplaceSummaryError};

fn error(source: impl std::error::Error + Send + Sync + 'static) -> SummaryStoreError {
    SummaryStoreError::Query {
        message: "summary conversion".into(),
        source: Box::new(source),
    }
}

async fn lock(conn: &mut PgConnection) -> Result<(bool, bool), sqlx::Error> {
    sqlx::query_as("SELECT enabled, inventory_complete FROM rh_summary_conversion FOR UPDATE")
        .fetch_one(conn)
        .await
}

async fn inventory(conn: &mut PgConnection, cutoff: Timestamp) -> Result<(), sqlx::Error> {
    let page: Vec<Uuid> = sqlx::query_scalar(
        "SELECT c.id FROM rh_conversations c CROSS JOIN rh_summary_conversion ctl
         WHERE ctl.inventory_after IS NULL OR c.id > ctl.inventory_after ORDER BY c.id LIMIT $1",
    )
    .bind(i64::from(INVENTORY_PAGE_SIZE))
    .fetch_all(&mut *conn)
    .await?;
    measure(conn, &page, cutoff).await?;
    sqlx::query(
        "UPDATE rh_summary_conversion
         SET inventory_after = coalesce($1, inventory_after), inventory_complete = $2",
    )
    .bind(page.last())
    .bind(page.len() < usize::from(INVENTORY_PAGE_SIZE))
    .execute(conn)
    .await?;
    Ok(())
}

async fn measure(
    conn: &mut PgConnection,
    page: &[Uuid],
    cutoff: Timestamp,
) -> Result<(), sqlx::Error> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO rh_summary_conversion_items
            (conversation_id, last_input_at, last_used_at, estimated_tokens, state)
         SELECT c.id, coalesce(s.last_input_at, source.last_input, c.created_at),
                greatest(source.last_used, summary_access.last_used), coalesce(source.estimate, 0),
                CASE WHEN s.settled_at IS NOT NULL OR coalesce(s.last_input_at, source.last_input, c.created_at) <= $2
                     THEN 'pending' ELSE 'waiting_quiet' END
         FROM rh_conversations c
         LEFT JOIN rh_summary_state s ON s.conversation_id = c.id
         CROSS JOIN LATERAL (
             SELECT max(m.created_at) AS last_input, greatest(max(a.last_accessed_at), max(m.last_accessed_at)) AS last_used,
                    sum((octet_length(m.content)::bigint + coalesce(octet_length(m.tool_result), 0)
                         + coalesce(octet_length(m.tool_args::text), 0) + 32) / 3)::bigint AS estimate
             FROM rh_messages m LEFT JOIN rh_message_access a ON a.message_id = m.id
             WHERE m.conversation_id = c.id
         ) source
         CROSS JOIN LATERAL (
             SELECT greatest(max(a.last_accessed_at), max(old.last_accessed_at)) AS last_used FROM rh_summaries old
             LEFT JOIN rh_summary_access a ON a.summary_id = old.id WHERE old.conversation_id = c.id
         ) summary_access
         WHERE c.id = ANY($1) AND {INCLUDED} AND (
           source.last_input IS NOT NULL OR EXISTS (
             SELECT 1 FROM rh_summaries old WHERE old.conversation_id = c.id AND old.kind IN ('leaf','branch','root')
           )
         )
           AND coalesce(s.historical_only, true)
           AND (s.summarized_through IS NULL OR s.summarized_through < s.input_revision)
         ON CONFLICT (conversation_id) DO UPDATE SET last_used_at = excluded.last_used_at,
            estimated_tokens = excluded.estimated_tokens, last_input_at = excluded.last_input_at
         WHERE rh_summary_conversion_items.state IN ('pending', 'waiting_quiet')"
    )))
    .bind(page).bind(crate::time::BindTime::bind_time(cutoff)).execute(&mut *conn).await?;
    Ok(())
}

async fn live_waiting(conn: &mut PgConnection) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT EXISTS (SELECT 1 FROM rh_summary_state s JOIN rh_conversations c ON c.id = s.conversation_id
         WHERE NOT s.historical_only AND s.settled_at IS NOT NULL
           AND s.input_revision IS DISTINCT FROM s.summarized_through
           AND s.input_revision IS DISTINCT FROM s.failed_revision AND {INCLUDED})"
    ))).fetch_one(conn).await
}

pub(crate) async fn can_publish(
    conn: &mut PgConnection,
    lease: ConversionLease,
) -> Result<bool, sqlx::Error> {
    let (enabled, _) = lock(conn).await?;
    Ok(enabled && valid(conn, lease).await? && !live_waiting(conn).await?)
}

async fn valid(conn: &mut PgConnection, lease: ConversionLease) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT EXISTS (SELECT 1 FROM rh_summary_conversion_items i
         JOIN rh_conversations c ON c.id = i.conversation_id
         JOIN rh_summary_state s ON s.conversation_id = c.id
         CROSS JOIN rh_summary_conversion ctl
         WHERE (i.estimated_tokens <= {GIANT_TOKENS} OR ctl.include_giants) AND c.id = $1 AND i.next_at = $2 AND i.reservation_expires_at > clock_timestamp()
           AND s.historical_only AND {0} AND {INCLUDED})",
        crate::summary_backend::ELIGIBLE.replace("$2", "$3")
    )))
    .bind(lease.conversation)
    .bind(crate::time::BindTime::bind_time(lease.claimed_at))
    .bind(lease.revision)
    .fetch_one(conn)
    .await
}

async fn bucket(conn: &mut PgConnection) -> Result<ConversionBucket, sqlx::Error> {
    let (rate, tokens, at) =
        sqlx::query_as("SELECT rate, bucket_tokens, bucket_at FROM rh_summary_conversion")
            .fetch_one(conn)
            .await?;
    Ok(ConversionBucket {
        rate,
        tokens,
        at: jiff_sqlx::Timestamp::to_jiff(at),
    })
}

async fn now(conn: &mut PgConnection) -> Result<Timestamp, sqlx::Error> {
    sqlx::query_scalar::<_, jiff_sqlx::Timestamp>("SELECT clock_timestamp()")
        .fetch_one(conn)
        .await
        .map(jiff_sqlx::Timestamp::to_jiff)
}

async fn clear_stale_items(conn: &mut PgConnection, clock: Timestamp) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE rh_summary_conversion_items SET state = 'pending', reservation_expires_at = NULL
            WHERE reservation_expires_at <= $1",
    )
    .bind(crate::time::BindTime::bind_time(clock))
    .execute(&mut *conn)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM rh_summary_conversion_items i USING rh_conversations c
             WHERE i.conversation_id = c.id AND i.state IN ('pending','waiting_quiet','failed') AND (NOT ({INCLUDED}) OR EXISTS (
                 SELECT 1 FROM rh_summary_state s WHERE s.conversation_id = c.id AND NOT s.historical_only))"
        ))).execute(&mut *conn).await?;
    Ok(())
}

fn read_progress(row: &sqlx::postgres::PgRow) -> Result<ConversionProgress, SummaryStoreError> {
    Ok(ConversionProgress {
        enabled: row.try_get("enabled").map_err(error)?,
        include_giants: row.try_get("include_giants").map_err(error)?,
        inventory_after: row.try_get("inventory_after").map_err(error)?,
        inventory_complete: row.try_get("inventory_complete").map_err(error)?,
        rate: row.try_get("rate").map_err(error)?,
        charged_tokens: u64::try_from(row.try_get::<i64, _>("charged_tokens").map_err(error)?)
            .map_err(error)?,
        provider_input_tokens: u64::try_from(
            row.try_get::<i64, _>("provider_input_tokens")
                .map_err(error)?,
        )
        .map_err(error)?,
        provider_output_tokens: u64::try_from(
            row.try_get::<i64, _>("provider_output_tokens")
                .map_err(error)?,
        )
        .map_err(error)?,
        ..ConversionProgress::default()
    })
}

#[async_trait::async_trait]
impl SummaryConversion for PostgresMemory {
    async fn control(
        &self,
        action: ConversionControl,
    ) -> Result<ConversionProgress, SummaryStoreError> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        lock(&mut tx).await.map_err(error)?;
        let query = match action {
            ConversionControl::Start { rate, include_giants } => sqlx::query(
                "UPDATE rh_summary_conversion SET enabled = true, include_giants = $1,
                 bucket_tokens = CASE WHEN charged_tokens = 0 AND bucket_tokens = 0 AND inventory_after IS NULL
                                      THEN coalesce($2, rate)::double precision ELSE bucket_tokens END,
                 rate = coalesce($2, rate), inventory_after = NULL, inventory_complete = false"
            ).bind(include_giants).bind(rate),
            ConversionControl::Resume { include_giants } => sqlx::query(
                "UPDATE rh_summary_conversion SET enabled = true, include_giants = include_giants OR $1"
            ).bind(include_giants),
            ConversionControl::Pause => sqlx::query("UPDATE rh_summary_conversion SET enabled = false"),
            ConversionControl::Retry { conversation } => sqlx::query(
                "UPDATE rh_summary_conversion_items SET state = 'pending', last_error = NULL
                 WHERE conversation_id = $1 AND state = 'failed'"
            ).bind(conversation),
            ConversionControl::Release { conversation } => {
                crate::summary_replace::exclusive(&mut tx, [conversation]).await.map_err(error)?;
                sqlx::query(RELEASE).bind(conversation)
            }
        };
        query.execute(&mut *tx).await.map_err(error)?;
        tx.commit().await.map_err(error)?;
        self.status().await
    }

    async fn status(&self) -> Result<ConversionProgress, SummaryStoreError> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(error)?;
        let row = sqlx::query("SELECT * FROM rh_summary_conversion")
            .fetch_one(&mut *tx)
            .await
            .map_err(error)?;
        let mut progress = read_progress(&row)?;
        let groups: Vec<(String, i64)> = sqlx::query_as(
            "SELECT state, count(*) FROM rh_summary_conversion_items GROUP BY state",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(error)?;
        for (state, count) in groups {
            progress.count(&state, count);
        }
        progress.giant_deferred = if progress.include_giants {
            0
        } else {
            sqlx::query_scalar(
                "SELECT count(*) FROM rh_summary_conversion_items
               WHERE estimated_tokens > $1 AND state IN ('pending','waiting_quiet')",
            )
            .bind(GIANT_TOKENS)
            .fetch_one(&mut *tx)
            .await
            .map_err(error)?
        };
        progress.current = sqlx::query_scalar(
            "SELECT conversation_id FROM rh_summary_conversion_items WHERE state = 'reserved'",
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(error)?;
        progress.recent_failures = sqlx::query_as(
            "SELECT conversation_id, last_error FROM rh_summary_conversion_items
             WHERE state = 'failed' ORDER BY next_at DESC LIMIT 10",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(error)?;
        tx.commit().await.map_err(error)?;
        Ok(progress)
    }

    async fn claim(
        &self,
        inactivity: SignedDuration,
    ) -> Result<Option<ConversionLease>, SummaryStoreError> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        let active: bool = sqlx::query_scalar("SELECT enabled FROM rh_summary_conversion")
            .fetch_one(&mut *tx)
            .await
            .map_err(error)?;
        if !active {
            return Ok(None);
        }
        let (enabled, scanned) = lock(&mut tx).await.map_err(error)?;
        if !enabled || live_waiting(&mut tx).await.map_err(error)? {
            return Ok(None);
        }
        let clock = now(&mut tx).await.map_err(error)?;
        let cutoff = kernel::memory::summary_conversion::inactivity_cutoff(clock, inactivity);
        if !scanned {
            inventory(&mut tx, cutoff).await.map_err(error)?;
            tx.commit().await.map_err(error)?;
            return Ok(None);
        }
        if bucket(&mut tx).await.map_err(error)?.at > clock {
            return Ok(None);
        }
        clear_stale_items(&mut tx, clock).await.map_err(error)?;
        let candidate: Option<(Uuid, jiff_sqlx::Timestamp, bool)> = sqlx::query_as(
            "SELECT c.id, i.last_input_at, coalesce(s.last_input_at <> i.last_input_at, false) FROM rh_summary_conversion_items i
             JOIN rh_conversations c ON c.id = i.conversation_id
             LEFT JOIN rh_summary_state s ON s.conversation_id = c.id
             CROSS JOIN rh_summary_conversion ctl
             WHERE i.state IN ('pending','waiting_quiet')
               AND NOT EXISTS (SELECT 1 FROM rh_summary_conversion_items WHERE state = 'reserved')
               AND (s.settled_at IS NOT NULL OR coalesce(s.last_input_at, i.last_input_at) <= $1)
               AND (i.estimated_tokens <= $2 OR (ctl.include_giants AND NOT EXISTS (
                   SELECT 1 FROM rh_summary_conversion_items normal WHERE normal.estimated_tokens <= $2
                     AND normal.state IN ('pending','waiting_quiet'))))
               AND coalesce(s.historical_only, true)
               AND (s.summarized_through IS NULL OR s.summarized_through < s.input_revision)
             ORDER BY (i.estimated_tokens > $2), greatest(i.last_input_at, i.last_used_at) DESC, i.conversation_id
             LIMIT 1 FOR UPDATE OF c SKIP LOCKED"
        ).bind(crate::time::BindTime::bind_time(cutoff)).bind(GIANT_TOKENS).fetch_optional(&mut *tx).await.map_err(error)?;
        let lease = if let Some((conversation, last_input, input_changed)) = candidate {
            if input_changed {
                measure(&mut tx, &[conversation], cutoff)
                    .await
                    .map_err(error)?;
                tx.commit().await.map_err(error)?;
                return Ok(None);
            }
            let revision = sqlx::query_scalar(
                "INSERT INTO rh_summary_state (conversation_id, input_revision, last_input_at, settled_at, historical_only)
                 VALUES ($1, 0, $2, $3, true) ON CONFLICT (conversation_id) DO UPDATE
                 SET settled_at = coalesce(rh_summary_state.settled_at, excluded.settled_at), unsettled_since = NULL
                 RETURNING input_revision"
            ).bind(conversation).bind(crate::time::BindTime::bind_time(last_input.to_jiff())).bind(crate::time::BindTime::bind_time(clock)).fetch_one(&mut *tx).await.map_err(error)?;
            sqlx::query(
                "UPDATE rh_summary_conversion_items SET state = 'reserved', next_at = $2,
                        reservation_expires_at = $3 WHERE conversation_id = $1",
            )
            .bind(conversation)
            .bind(crate::time::BindTime::bind_time(clock))
            .bind(crate::time::BindTime::bind_time(
                kernel::memory::summary_conversion::bounded_deadline(clock, LEASE),
            ))
            .execute(&mut *tx)
            .await
            .map_err(error)?;
            Some(ConversionLease {
                conversation,
                revision,
                claimed_at: clock,
            })
        } else {
            None
        };
        tx.commit().await.map_err(error)?;
        Ok(lease)
    }

    async fn admit(
        &self,
        lease: ConversionLease,
        charge: Option<ConversionCharge>,
    ) -> Result<bool, SummaryStoreError> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        if !can_publish(&mut tx, lease).await.map_err(error)? {
            return Ok(false);
        }
        let clock = now(&mut tx).await.map_err(error)?;
        if let Some(charge) = charge {
            let mut bucket = bucket(&mut tx).await.map_err(error)?;
            let admitted = bucket.debit(charge, clock);
            sqlx::query(
                "UPDATE rh_summary_conversion SET bucket_tokens = $1, bucket_at = $2,
                         charged_tokens = charged_tokens + $3",
            )
            .bind(bucket.tokens)
            .bind(crate::time::BindTime::bind_time(bucket.at))
            .bind(if admitted { charge.estimate } else { 0 })
            .execute(&mut *tx)
            .await
            .map_err(error)?;
            if !admitted {
                tx.commit().await.map_err(error)?;
                return Ok(false);
            }
        }
        sqlx::query("UPDATE rh_summary_conversion_items SET reservation_expires_at = $2 WHERE conversation_id = $1")
            .bind(lease.conversation).bind(crate::time::BindTime::bind_time(kernel::memory::summary_conversion::bounded_deadline(clock, LEASE))).execute(&mut *tx).await.map_err(error)?;
        tx.commit().await.map_err(error)?;
        Ok(true)
    }

    async fn freeze(
        &self,
        lease: ConversionLease,
        root: Option<Uuid>,
        manifest: SummaryArtifact,
    ) -> Result<Option<Uuid>, SummaryStoreError> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        if !can_publish(&mut tx, lease).await.map_err(error)? {
            return Ok(None);
        }
        crate::summary_replace::exclusive(&mut tx, [lease.conversation])
            .await
            .map_err(error)?;
        if !can_publish(&mut tx, lease).await.map_err(error)? {
            return Ok(None);
        }
        let id = crate::summary_backend::freeze(
            &mut tx,
            lease.conversation,
            lease.revision,
            root,
            manifest,
        )
        .await
        .map_err(error)?;
        tx.commit().await.map_err(error)?;
        Ok(Some(id))
    }

    async fn try_model_slot(
        &self,
        limit: u32,
    ) -> Result<Option<Box<dyn SummaryPermit>>, SummaryStoreError> {
        use sqlx::Connection as _;
        if limit < 2 {
            return Ok(None);
        }
        let slot = i32::try_from(limit - 1).map_err(|_| SummaryStoreError::Count)?;
        let mut conn = PgConnection::connect_with(&self.pool.connect_options())
            .await
            .map_err(error)?;
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(1937075568, $1)")
            .bind(slot)
            .fetch_one(&mut conn)
            .await
            .map_err(error)?;
        Ok(locked.then(|| Box::new(conn) as Box<dyn SummaryPermit>))
    }

    async fn report(&self, usage: ConversionUsage) -> Result<(), SummaryStoreError> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        lock(&mut tx).await.map_err(error)?;
        let mut bucket = bucket(&mut tx).await.map_err(error)?;
        bucket.reconcile(usage);
        sqlx::query("UPDATE rh_summary_conversion SET bucket_tokens = $1, bucket_at = $2,
                     charged_tokens = charged_tokens + $3 - $4, provider_input_tokens = provider_input_tokens + $3,
                     provider_output_tokens = provider_output_tokens + $5")
            .bind(bucket.tokens).bind(crate::time::BindTime::bind_time(bucket.at)).bind(usage.input).bind(usage.estimate).bind(usage.output)
            .execute(&mut *tx).await.map_err(error)?;
        tx.commit().await.map_err(error)
    }

    async fn finish(
        &self,
        lease: ConversionLease,
        outcome: ConversionOutcome,
    ) -> Result<(), SummaryStoreError> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        lock(&mut tx).await.map_err(error)?;
        let (state, failure) = match outcome {
            ConversionOutcome::Yield => ("pending", None),
            ConversionOutcome::Complete => ("complete", None),
            ConversionOutcome::Fail(error) => ("failed", Some(error)),
        };
        sqlx::query(
            "UPDATE rh_summary_conversion_items SET state = $3, reservation_expires_at = NULL,
                     last_error = $4, next_at = clock_timestamp()
                     WHERE conversation_id = $1 AND next_at = $2 AND state = 'reserved'",
        )
        .bind(lease.conversation)
        .bind(crate::time::BindTime::bind_time(lease.claimed_at))
        .bind(state)
        .bind(failure)
        .execute(&mut *tx)
        .await
        .map_err(error)?;
        tx.commit().await.map_err(error)
    }

    async fn publish(
        &self,
        lease: ConversionLease,
        tree: PreparedSummaryTree,
        indexes: SummaryIndexes,
    ) -> Result<bool, SummaryStoreError> {
        if tree.conversation != lease.conversation
            || tree.expected_revision != Some(lease.revision)
            || !tree.historical
        {
            return Ok(false);
        }
        match self
            .replace_conversion_summary(&tree, &indexes, lease)
            .await
        {
            Ok(()) => Ok(true),
            Err(ReplaceSummaryError::Reference(_)) => Ok(false),
            Err(failure) => Err(error(failure)),
        }
    }
}

const RELEASE: &str = "WITH part_order AS (
    SELECT part.id, max(root_link.ordinal) AS last_position, min(root_link.ordinal) AS first_position
    FROM rh_summaries part JOIN rh_summary_sources link ON link.summary_id = part.id
    JOIN rh_summary_sources root_link ON root_link.summary_id = part.parent_id
        AND root_link.message_id = link.message_id
    WHERE part.conversation_id = $1 AND part.kind = 'part'
    GROUP BY part.id
), anchors AS (
    SELECT DISTINCT ON (src.message_id) src.message_id, s.id
    FROM rh_summaries s JOIN rh_summary_sources src ON src.summary_id = s.id
    LEFT JOIN part_order p ON p.id = s.id
    WHERE s.conversation_id = $1 AND s.kind IN ('session','part')
    AND (s.kind = 'part' OR NOT EXISTS (SELECT 1 FROM rh_summaries part
         WHERE part.parent_id = s.id AND part.kind = 'part'))
    ORDER BY src.message_id, (s.kind = 'session'), p.last_position NULLS LAST, p.first_position NULLS LAST, s.issued_part_ordinal, s.id
)
UPDATE rh_summary_deliveries d SET anchor_id = a.id, historical_backlog = false
FROM anchors a WHERE d.conversation_id = $1 AND d.message_id = a.message_id
  AND d.historical_backlog";
