mod release;

use jiff::SignedDuration;
use jiff::Timestamp;
use kernel::memory::summary_backend::SummaryArtifact;
use kernel::memory::{
    SummaryStoreError,
    summary_backend::{SummaryIndexes, SummaryPermit},
    summary_conversion::{
        ConversionBucket, ConversionCharge, ConversionControl, ConversionLease, ConversionOutcome,
        ConversionProgress, ConversionUsage, GIANT_TOKENS, INVENTORY_PAGE_SIZE, SummaryConversion,
    },
    summary_reference::PreparedSummaryTree,
};
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use wire::time::TimestampFormat as _;

use crate::{SqliteError, SqliteStore, settlement::INCLUDED, summary_port::parse_id, time};

fn error(source: rusqlite::Error) -> SqliteError {
    SqliteError::query("summary conversion")(source)
}

fn store_error(source: SqliteError) -> SummaryStoreError {
    SummaryStoreError::Query {
        message: "summary conversion".into(),
        source: Box::new(source),
    }
}

fn immediate(conn: &mut Connection) -> Result<Transaction<'_>, SqliteError> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(error)
}

fn inventory(conn: &Connection, cutoff: Timestamp) -> Result<(), SqliteError> {
    let page = conn
        .prepare(
            "SELECT c.id FROM ros_conversations c CROSS JOIN rh_summary_conversion ctl
         WHERE ctl.inventory_after IS NULL OR c.id > ctl.inventory_after ORDER BY c.id LIMIT ?1",
        )
        .and_then(|mut statement| {
            statement
                .query_map([INVENTORY_PAGE_SIZE], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(error)?;
    measure(conn, &page, cutoff)?;
    conn.execute(
        "UPDATE rh_summary_conversion
         SET inventory_after = coalesce(?1, inventory_after), inventory_complete = ?2",
        params![page.last(), page.len() < usize::from(INVENTORY_PAGE_SIZE)],
    )
    .map_err(error)?;
    Ok(())
}

fn measure(conn: &Connection, page: &[String], cutoff: Timestamp) -> Result<(), SqliteError> {
    let ids = serde_json::to_string(page).map_err(|source| SqliteError::Encode {
        column: "conversion inventory page",
        source,
    })?;
    conn.execute(&format!(
        "WITH source AS (
            SELECT m.conversation_id, max(rh_instant(m.created_at)) AS last_input,
                   max(rh_instant(m.last_accessed_at)) AS last_used,
                   sum((coalesce(octet_length(m.content), 0) + coalesce(octet_length(m.tool_result), 0)
                        + coalesce(octet_length(m.tool_args), 0) + 32) / 3) AS estimate
            FROM ros_messages m WHERE m.conversation_id IN (SELECT value FROM json_each(?1))
            GROUP BY m.conversation_id
         )
         INSERT INTO rh_summary_conversion_items
            (conversation_id, last_input_at, last_used_at, estimated_tokens, state, next_at)
         SELECT c.id, coalesce(s.last_input_at, clock.captured_at, rh_timestamp(source.last_input), c.created_at),
                rh_timestamp(nullif(max(coalesce(source.last_used, ''), coalesce(
                    (SELECT max(rh_instant(old.last_accessed_at)) FROM ros_summaries old WHERE old.conversation_id = c.id), '')), '')),
                coalesce(source.estimate, 0),
                CASE WHEN s.settled_at IS NOT NULL OR coalesce(rh_instant(s.last_input_at), rh_instant(clock.captured_at), source.last_input, rh_instant(c.created_at)) <= ?3
                     THEN 'pending' ELSE 'waiting_quiet' END, ?2
         FROM ros_conversations c LEFT JOIN source ON c.id = source.conversation_id
         LEFT JOIN rh_summary_state s ON s.conversation_id = c.id
         LEFT JOIN rh_summary_capture_clock clock ON clock.conversation_id = c.id
         WHERE c.id IN (SELECT value FROM json_each(?1)) AND (
           source.last_input IS NOT NULL OR EXISTS (
             SELECT 1 FROM ros_summaries old WHERE old.conversation_id = c.id AND old.kind IN ('leaf','branch','root')
           )
         ) AND {INCLUDED} AND coalesce(s.historical_only, 1)
           AND (s.summarized_through IS NULL OR s.summarized_through < s.input_revision)
         ON CONFLICT (conversation_id) DO UPDATE SET last_used_at = excluded.last_used_at,
            estimated_tokens = excluded.estimated_tokens, last_input_at = excluded.last_input_at
         WHERE rh_summary_conversion_items.state IN ('pending','waiting_quiet')"
    ), params![ids, time::stamp(cutoff), time::seek_key(cutoff)]).map_err(error)?;
    Ok(())
}

fn live_waiting(conn: &Connection) -> Result<bool, SqliteError> {
    conn.query_row(&format!(
        "SELECT EXISTS (SELECT 1 FROM rh_summary_state s JOIN ros_conversations c ON c.id = s.conversation_id
         WHERE NOT s.historical_only AND s.settled_at IS NOT NULL
           AND s.input_revision IS NOT s.summarized_through
           AND s.input_revision IS NOT s.failed_revision AND {INCLUDED})"
    ), [], |row| row.get(0)).map_err(error)
}

pub(crate) fn can_publish(conn: &Connection, lease: ConversionLease) -> Result<bool, SqliteError> {
    Ok(valid(conn, lease, Timestamp::now(), true)? && !live_waiting(conn)?)
}

fn valid(
    conn: &Connection,
    lease: ConversionLease,
    clock: Timestamp,
    publishing: bool,
) -> Result<bool, SqliteError> {
    if clock < lease.claimed_at {
        return Ok(false);
    }
    conn
        .query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM rh_summary_conversion_items i
         JOIN ros_conversations c ON c.id = i.conversation_id
         JOIN rh_summary_state s ON s.conversation_id = c.id
         CROSS JOIN rh_summary_conversion ctl
         WHERE (NOT ?5 OR ctl.enabled) AND (i.estimated_tokens <= {GIANT_TOKENS} OR ctl.include_giants) AND c.id = ?1 AND i.next_at = ?2 AND i.reservation_expires_at > ?3
           AND s.historical_only AND s.input_revision = ?4 AND s.settled_at IS NOT NULL
           AND s.summarized_through IS NOT ?4 AND s.failed_revision IS NOT ?4 AND {INCLUDED})"
            ),
            params![
                lease.conversation.to_string(),
                claim_stamp(lease.claimed_at),
                time::stamp(clock),
                lease.revision,
                publishing
            ],
            |row| row.get::<_, bool>(0),
        )
        .map_err(error)
}

fn claim_stamp(at: Timestamp) -> String {
    at.rfc3339_nanos()
}

fn bucket(conn: &Connection) -> Result<ConversionBucket, SqliteError> {
    let (rate, tokens, at): (i64, f64, String) = conn
        .query_row(
            "SELECT rate, bucket_tokens, bucket_at FROM rh_summary_conversion",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(error)?;
    Ok(ConversionBucket {
        rate,
        tokens,
        at: time::stored(&at, "conversion bucket time")?,
    })
}

fn read_token_count(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<u64> {
    let count: i64 = row.get(column)?;
    u64::try_from(count).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, count))
}

fn read_status(conn: &Connection) -> Result<ConversionProgress, SqliteError> {
    let (mut progress, cursor) = conn.query_row(
        "SELECT enabled, include_giants, inventory_complete, rate, charged_tokens,
                provider_input_tokens, provider_output_tokens, inventory_after FROM rh_summary_conversion",
        [],
        |row| Ok((ConversionProgress {
            enabled: row.get(0)?,
            include_giants: row.get(1)?,
            inventory_complete: row.get(2)?,
            rate: row.get(3)?,
            charged_tokens: read_token_count(row, 4)?,
            provider_input_tokens: read_token_count(row, 5)?,
            provider_output_tokens: read_token_count(row, 6)?,
            ..ConversionProgress::default()
        }, row.get::<_, Option<String>>(7)?)),
    ).map_err(error)?;
    progress.inventory_after = cursor.as_deref().map(parse_id).transpose()?;
    let mut statement = conn
        .prepare("SELECT state, count(*) FROM rh_summary_conversion_items GROUP BY state")
        .map_err(error)?;
    for row in statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?)))
        .map_err(error)?
    {
        let (state, count) = row.map_err(error)?;
        progress.count(&state, count);
    }
    progress.waiting_second_pass = conn
        .query_row(
            "SELECT count(*) FROM rh_summary_conversion_items
         WHERE second_pass_at IS NOT NULL AND state IN ('pending','waiting_quiet')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !progress.include_giants {
        progress.giant_deferred = conn.query_row(
            "SELECT count(*) FROM rh_summary_conversion_items WHERE estimated_tokens > ?1 AND state IN ('pending','waiting_quiet')",
            [GIANT_TOKENS], |row| row.get(0)
        ).map_err(error)?;
    }
    progress.current = conn
        .query_row(
            "SELECT conversation_id FROM rh_summary_conversion_items WHERE state = 'reserved'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(error)?
        .as_deref()
        .map(parse_id)
        .transpose()?;
    let mut recent = conn
        .prepare(
            "SELECT conversation_id, last_error FROM rh_summary_conversion_items
             WHERE state = 'failed' ORDER BY next_at DESC LIMIT 10",
        )
        .map_err(error)?;
    progress.recent_failures = recent
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?)))
        .map_err(error)?
        .map(|row| {
            let (id, failure) = row.map_err(error)?;
            Ok((parse_id(&id)?, failure))
        })
        .collect::<Result<_, SqliteError>>()?;
    Ok(progress)
}

#[async_trait::async_trait]
impl SummaryConversion for SqliteStore {
    async fn control(
        &self,
        action: ConversionControl,
    ) -> Result<ConversionProgress, SummaryStoreError> {
        let has_opening = self.with_conn(move |conn| {
            let tx = immediate(conn)?;
            match action {
                ConversionControl::ArmOpening { conversation } => {
                    let has_opening = crate::title::arm_historical(&tx, conversation)?;
                    tx.commit().map_err(error)?;
                    return Ok(has_opening);
                }
                ConversionControl::Start { rate, include_giants } => tx.execute(
                    "UPDATE rh_summary_conversion SET enabled = 1, include_giants = ?1, rate = coalesce(?2, rate),
                     bucket_tokens = CASE WHEN charged_tokens = 0 AND bucket_tokens = 0 AND inventory_after IS NULL
                                          THEN coalesce(?2, rate) ELSE bucket_tokens END,
                     bucket_at = CASE WHEN julianday(bucket_at) > julianday(?3) THEN bucket_at ELSE ?3 END,
                     inventory_after = NULL, inventory_complete = 0",
                    params![include_giants, rate, time::stamp(Timestamp::now())],
                ),
                ConversionControl::Resume { include_giants } => tx.execute(
                    "UPDATE rh_summary_conversion SET enabled = 1, include_giants = include_giants OR ?1,
                     bucket_at = CASE WHEN julianday(bucket_at) > julianday(?2) THEN bucket_at ELSE ?2 END",
                    params![include_giants, time::stamp(Timestamp::now())],
                ),
                ConversionControl::Pause => tx.execute("UPDATE rh_summary_conversion SET enabled = 0", []),
                ConversionControl::Retry { conversation } => tx.execute(
                    "UPDATE rh_summary_conversion_items SET state = 'pending', last_error = NULL, timeout_retries = 0, second_pass_at = NULL
                     WHERE conversation_id = ?1 AND state = 'failed'", [conversation.to_string()],
                ),
                ConversionControl::Release { conversation } => {
                    Ok(release::deliveries(&tx, &conversation.to_string())?)
                }
            }.map_err(error)?;
            tx.commit().map_err(error)?;
            Ok(true)
        }).await.map_err(store_error)?;
        if !has_opening && let ConversionControl::ArmOpening { conversation } = action {
            return Err(SummaryStoreError::NoOpening { conversation });
        }
        self.status().await
    }

    async fn status(&self) -> Result<ConversionProgress, SummaryStoreError> {
        self.with_conn(|conn| {
            let tx = conn.transaction().map_err(error)?;
            read_status(&tx)
        })
        .await
        .map_err(store_error)
    }

    async fn claim_with_expiry(
        &self,
        inactivity: SignedDuration,
        expiry: SignedDuration,
    ) -> Result<Option<ConversionLease>, SummaryStoreError> {
        self.with_conn(move |conn| {
            let active: bool = conn.query_row("SELECT enabled FROM rh_summary_conversion", [], |row| row.get(0)).map_err(error)?;
            if !active {
                return Ok(None);
            }
            let tx = immediate(conn)?;
            let (enabled, scanned): (bool, bool) = tx.query_row(
                "SELECT enabled, inventory_complete FROM rh_summary_conversion", [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).map_err(error)?;
            if !enabled || live_waiting(&tx)? {
                return Ok(None);
            }
            let clock = Timestamp::now();
            let cutoff = kernel::memory::summary_conversion::inactivity_cutoff(clock, inactivity);
            if !scanned {
                inventory(&tx, cutoff)?;
                tx.commit().map_err(error)?;
                return Ok(None);
            }
            tx.execute("UPDATE rh_summary_conversion_items SET state = 'pending', reservation_expires_at = NULL
                         WHERE reservation_expires_at <= ?1", [time::stamp(clock)]).map_err(error)?;
            if bucket(&tx)?.at > clock {
                tx.commit().map_err(error)?;
                return Ok(None);
            }
            tx.execute(&format!(
                "DELETE FROM rh_summary_conversion_items
                 WHERE state IN ('pending','waiting_quiet','failed') AND conversation_id IN (
                    SELECT c.id FROM ros_conversations c LEFT JOIN rh_summary_state s ON s.conversation_id = c.id
                    WHERE NOT ({INCLUDED}) OR NOT s.historical_only)"
             ), []).map_err(error)?;
            let candidate: Option<(String, String, bool, i32, bool)> = tx.query_row(
                "SELECT i.conversation_id, i.last_input_at, coalesce(rh_instant(coalesce(s.last_input_at, clock.captured_at)) <> rh_instant(i.last_input_at), 0), i.timeout_retries, i.second_pass_at IS NOT NULL FROM rh_summary_conversion_items i
                 LEFT JOIN rh_summary_state s ON s.conversation_id = i.conversation_id
                 LEFT JOIN rh_summary_capture_clock clock ON clock.conversation_id = i.conversation_id
                 CROSS JOIN rh_summary_conversion ctl
                 WHERE i.state IN ('pending','waiting_quiet')
                   AND NOT EXISTS (SELECT 1 FROM rh_summary_conversion_items WHERE state = 'reserved')
                   AND (s.settled_at IS NOT NULL OR rh_instant(coalesce(s.last_input_at, clock.captured_at, i.last_input_at)) <= ?1)
                   AND (i.estimated_tokens <= ?2 OR (ctl.include_giants AND NOT EXISTS (
                       SELECT 1 FROM rh_summary_conversion_items normal WHERE normal.estimated_tokens <= ?2
                         AND normal.state IN ('pending','waiting_quiet')
                     AND (i.second_pass_at IS NOT NULL OR normal.second_pass_at IS NULL))))
                   AND coalesce(s.historical_only, 1)
                   AND (s.summarized_through IS NULL OR s.summarized_through < s.input_revision)
                 ORDER BY (i.second_pass_at IS NOT NULL), i.second_pass_at, (i.estimated_tokens > ?2), max(rh_instant(i.last_input_at), rh_instant(coalesce(i.last_used_at, i.last_input_at))) DESC, i.conversation_id
                 LIMIT 1",
                params![time::seek_key(cutoff), GIANT_TOKENS], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            ).optional().map_err(error)?;
            let lease = if let Some((id, last_input, input_changed, timeout_retries, second_pass)) = candidate {
                if input_changed {
                    measure(&tx, &[id], cutoff)?;
                    tx.commit().map_err(error)?;
                    return Ok(None);
                }
                let revision = tx.query_row(
                    "INSERT INTO rh_summary_state (conversation_id, input_revision, last_input_at, settled_at, historical_only)
                     VALUES (?1, 0, ?2, ?3, 1) ON CONFLICT (conversation_id) DO UPDATE
                     SET settled_at = coalesce(rh_summary_state.settled_at, excluded.settled_at), unsettled_since = NULL
                     RETURNING input_revision",
                    params![id, last_input, time::stamp(clock)], |row| row.get(0)
                ).map_err(error)?;
                tx.execute("UPDATE rh_summary_conversion_items SET state = 'reserved', next_at = ?2,
                            reservation_expires_at = ?3 WHERE conversation_id = ?1",
                           params![id, claim_stamp(clock), time::stamp(kernel::memory::summary_conversion::bounded_deadline(clock, expiry))]).map_err(error)?;
                Some(ConversionLease { conversation: parse_id(&id)?, revision, claimed_at: clock, timeout_retries, second_pass })
            } else {
                None
            };
            tx.commit().map_err(error)?;
            Ok(lease)
        }).await.map_err(store_error)
    }

    async fn renew(
        &self,
        lease: ConversionLease,
        expiry: SignedDuration,
    ) -> Result<bool, SummaryStoreError> {
        self.with_conn(move |conn| {
            let tx = immediate(conn)?;
            let clock = Timestamp::now();
            if !valid(&tx, lease, clock, false)? {
                return Ok(false);
            }
            tx.execute("UPDATE rh_summary_conversion_items SET reservation_expires_at = ?2 WHERE conversation_id = ?1",
                params![lease.conversation.to_string(), time::stamp(kernel::memory::summary_conversion::bounded_deadline(clock, expiry))]).map_err(error)?;
            tx.commit().map_err(error)?;
            Ok(true)
        }).await.map_err(store_error)
    }

    async fn admit(
        &self,
        lease: ConversionLease,
        charge: Option<ConversionCharge>,
    ) -> Result<bool, SummaryStoreError> {
        self.with_conn(move |conn| {
            let tx = immediate(conn)?;
            if !can_publish(&tx, lease)? {
                return Ok(false);
            }
            let clock = Timestamp::now();
            if let Some(charge) = charge {
                let mut bucket = bucket(&tx)?;
                let admitted = bucket.debit(charge, clock);
                tx.execute(
                    "UPDATE rh_summary_conversion SET bucket_tokens = ?1, bucket_at = ?2,
                         charged_tokens = charged_tokens + ?3",
                    params![
                        bucket.tokens,
                        time::stamp(bucket.at),
                        if admitted { charge.estimate } else { 0 }
                    ],
                )
                .map_err(error)?;
                if !admitted {
                    tx.commit().map_err(error)?;
                    return Ok(false);
                }
            }
            tx.commit().map_err(error)?;
            Ok(true)
        })
        .await
        .map_err(store_error)
    }

    async fn freeze(
        &self,
        lease: ConversionLease,
        root: Option<uuid::Uuid>,
        manifest: SummaryArtifact,
    ) -> Result<Option<uuid::Uuid>, SummaryStoreError> {
        self.with_conn(move |conn| {
            let tx = immediate(conn)?;
            if !can_publish(&tx, lease)? {
                return Ok(None);
            }
            let id = crate::summary_backend::freeze(
                &tx,
                lease.conversation,
                lease.revision,
                root,
                &manifest,
            )?;
            tx.commit().map_err(error)?;
            Ok(Some(id))
        })
        .await
        .map_err(store_error)
    }

    async fn try_model_slot(
        &self,
        limit: u32,
    ) -> Result<Option<Box<dyn SummaryPermit>>, SummaryStoreError> {
        Ok((limit > 1).then(|| Box::new(()) as Box<dyn SummaryPermit>))
    }

    async fn live_pending(&self) -> Result<bool, SummaryStoreError> {
        self.with_conn(|conn| live_waiting(conn))
            .await
            .map_err(store_error)
    }

    async fn report(&self, usage: ConversionUsage) -> Result<(), SummaryStoreError> {
        self.with_conn(move |conn| {
            let tx = immediate(conn)?;
            let mut bucket = bucket(&tx)?;
            let adjustment = bucket.reconcile(usage);
            tx.execute("UPDATE rh_summary_conversion SET bucket_tokens = ?1, bucket_at = ?2,
                         charged_tokens = charged_tokens + ?5, provider_input_tokens = provider_input_tokens + ?3,
                         provider_output_tokens = provider_output_tokens + ?4",
                params![bucket.tokens, time::stamp(bucket.at), usage.input, usage.output, adjustment],
            ).map_err(error)?;
            tx.commit().map_err(error)
        }).await.map_err(store_error)
    }

    async fn finish(
        &self,
        lease: ConversionLease,
        outcome: ConversionOutcome,
    ) -> Result<(), SummaryStoreError> {
        self.with_conn(move |conn| {
            let requeue = matches!(outcome, ConversionOutcome::Requeue);
            let (state, failure, retries) = outcome.status(lease);
            conn.execute(
                "UPDATE rh_summary_conversion_items SET state = ?3, reservation_expires_at = NULL,
                         last_error = ?4, next_at = ?5, timeout_retries = ?6,
                         second_pass_at = CASE WHEN ?7 THEN ?5 ELSE second_pass_at END
                         WHERE conversation_id = ?1 AND next_at = ?2 AND state = 'reserved'",
                params![
                    lease.conversation.to_string(),
                    claim_stamp(lease.claimed_at),
                    state,
                    failure,
                    claim_stamp(Timestamp::now()),
                    retries,
                    requeue
                ],
            )
            .map_err(error)?;
            Ok(())
        })
        .await
        .map_err(store_error)
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
        match self.replace_conversion_summary(tree, indexes, lease).await {
            Ok(()) => Ok(true),
            Err(SqliteError::SummaryReference(_)) => Ok(false),
            Err(failure) => Err(store_error(failure)),
        }
    }
}
