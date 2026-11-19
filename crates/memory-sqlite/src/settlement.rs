use jiff::Timestamp;
use kernel::memory::settlement::{
    SETTLEMENT_PAGE_SIZE, SettleResult, SettleStatus, SettlementPage, SummaryState,
};
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};
use uuid::Uuid;
use wire::capture::SessionObservation;
use wire::time::TimestampFormat;

use crate::summary_port::parse_id;
use crate::{SqliteError, SqliteStore, time};

pub(crate) const INCLUDED: &str = "c.session_key NOT LIKE 'heartbeat:%'
    AND NOT EXISTS (SELECT 1 FROM rh_session_trash t WHERE t.session_key = c.session_key)
    AND NOT EXISTS (SELECT 1 FROM rh_session_tombstones t WHERE t.session_key = c.session_key)";

fn seed(conn: &Connection, id: &str, fresh: Option<Timestamp>) -> Result<(), SqliteError> {
    let last = if let Some(at) = fresh {
        at
    } else {
        let at: String = conn.query_row(
                "SELECT coalesce(clock.captured_at,
                            (SELECT m.created_at FROM ros_messages m WHERE m.conversation_id = c.id
                             ORDER BY rh_instant(m.created_at) DESC LIMIT 1), c.created_at)
                   FROM ros_conversations c LEFT JOIN rh_summary_capture_clock clock ON clock.conversation_id = c.id
                   WHERE c.id = ?1",
                [id], |row| row.get(0),
            ).map_err(failure)?;
        time::stored(&at, "last source time")?
    };
    conn.execute(
        "INSERT INTO rh_summary_state
            (conversation_id, input_revision, last_input_at, unsettled_since, historical_only)
         VALUES (?1, 0, ?2, ?2, true) ON CONFLICT DO NOTHING",
        params![id, last.rfc3339_nanos()],
    )
    .map_err(failure)?;
    Ok(())
}

fn state(conn: &Connection, id: &str) -> Result<Option<SummaryState>, SqliteError> {
    conn.query_row(
        "SELECT input_revision, last_input_at, unsettled_since, settled_at,
                summarized_through, failed_revision, historical_only, last_observed_event_at,
                last_observed_end_at, last_observed_end_key
           FROM rh_summary_state WHERE conversation_id = ?1",
        [id],
        |row| {
            Ok((
                row.get(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get(9)?,
            ))
        },
    )
    .optional()
    .map_err(failure)?
    .map(|row| {
        let stamp =
            |value: Option<String>, column| value.map(|at| time::stored(&at, column)).transpose();
        Ok(SummaryState {
            conversation_id: parse_id(id)?,
            input_revision: row.0,
            last_input_at: time::stored(&row.1, "last_input_at")?,
            unsettled_since: stamp(row.2, "unsettled_since")?,
            settled_at: stamp(row.3, "settled_at")?,
            summarized_through: row.4,
            failed_revision: row.5,
            historical_only: row.6,
            last_observed_event_at: stamp(row.7, "last_observed_event_at")?,
            last_observed_end_at: stamp(row.8, "last_observed_end_at")?,
            last_observed_end_key: row.9,
        })
    })
    .transpose()
}

fn save(conn: &Connection, state: &SummaryState) -> Result<(), SqliteError> {
    conn.execute(
        "UPDATE rh_summary_state SET input_revision = ?2, last_input_at = ?3,
            unsettled_since = ?4, settled_at = ?5, failed_revision = ?6,
            historical_only = ?7, last_observed_event_at = ?8,
            last_observed_end_at = ?9, last_observed_end_key = ?10
          WHERE conversation_id = ?1",
        params![
            state.conversation_id.to_string(),
            state.input_revision,
            state.last_input_at.rfc3339_z(),
            state.unsettled_since.map(TimestampFormat::rfc3339_z),
            state.settled_at.map(TimestampFormat::rfc3339_z),
            state.failed_revision,
            state.historical_only,
            state.last_observed_event_at.map(observation_stamp),
            state.last_observed_end_at.map(observation_stamp),
            state.last_observed_end_key,
        ],
    )
    .map_err(failure)?;
    Ok(())
}

fn observation_stamp(at: Timestamp) -> String {
    at.rfc3339_nanos()
}

pub(crate) fn prior_observation(
    conn: &Connection,
    id: &str,
    key: &str,
) -> Result<Option<SessionObservation>, SqliteError> {
    let settings: Option<Option<String>> = conn
        .query_row(
            "SELECT c.settings FROM ros_conversations c
         LEFT JOIN rh_summary_state s ON s.conversation_id = c.id
         WHERE c.id = ?1 AND s.last_observed_event_at IS NULL",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(failure)?;
    let settings = settings
        .flatten()
        .and_then(|text| match serde_json::from_str(&text) {
            Ok(settings) => Some(settings),
            Err(error) => {
                tracing::debug!(error_kind = ?error.classify(), "stored capture settings are unreadable");
                None
            }
        });
    Ok(capture_observation(key, settings.as_ref(), None))
}

pub(crate) fn record_capture(
    conn: &Connection,
    id: &str,
    now: Timestamp,
    changed: bool,
    imported: bool,
    (observation, prior): (Option<&SessionObservation>, Option<&SessionObservation>),
) -> Result<(), SqliteError> {
    let now = now.truncate_subseconds(3);
    if changed {
        conn.execute(
            "INSERT INTO rh_summary_capture_clock (conversation_id, captured_at) VALUES (?1, ?2)
             ON CONFLICT (conversation_id) DO UPDATE SET captured_at = excluded.captured_at",
            params![id, time::stamp(now)],
        )
        .map_err(failure)?;
    }
    let mut current = state(conn, id)?;
    if current.is_none() {
        seed(conn, id, changed.then_some(now))?;
        current = state(conn, id)?;
    }
    if let Some(mut current) = current {
        let seeded = current.record_capture(now, false, imported, prior.map(|o| (o.at, None)));
        let updated = current.record_capture(
            now,
            changed,
            imported,
            observation.map(|o| (o.at, o.end_key.as_deref())),
        );
        if seeded || updated {
            save(conn, &current)?;
        }
    }
    Ok(())
}

impl SqliteStore {
    pub(crate) async fn mark_settled(
        &self,
        session: &str,
        owner: &str,
    ) -> Result<Option<SettleResult>, SqliteError> {
        let session = session.to_owned();
        let id = kernel::memory::settlement::session_uuid(&session).map(|id| id.to_string());
        let owner = owner.to_owned();
        let now = time::stored(&time::stamp(self.now()), "settled_at")?;
        self.with_conn(move |conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(failure)?;
            let found = tx
                .prepare(
                    "SELECT id FROM ros_conversations
                 WHERE owner_user_id = ?1 AND (id = ?2 OR session_key = ?3) ORDER BY id LIMIT 2",
                )
                .map_err(failure)?
                .query_map(params![owner, id, session], |row| row.get::<_, String>(0))
                .map_err(failure)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(failure)?;
            let [id] = found.as_slice() else {
                return Ok(None);
            };
            let mut current = state(&tx, id)?;
            if current.is_none() {
                seed(&tx, id, None)?;
                current = state(&tx, id)?;
            }
            let Some(mut current) = current else {
                return Ok(None);
            };
            let previously_failed = current.failed_revision.is_some();
            let result = current.manual_settle(now);
            if result.status == SettleStatus::Settled || previously_failed {
                save(&tx, &current)?;
            }
            tx.commit().map_err(failure)?;
            Ok(Some(result))
        })
        .await
    }

    pub(crate) async fn read_summary_state(
        &self,
        id: Uuid,
    ) -> Result<Option<SummaryState>, SqliteError> {
        self.with_conn(move |conn| state(conn, &id.to_string()))
            .await
    }

    pub(crate) async fn eligible_summary(
        &self,
        id: Uuid,
        revision: i64,
    ) -> Result<bool, SqliteError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM rh_summary_state s
                 JOIN ros_conversations c ON c.id = s.conversation_id
                 WHERE c.id = ?1 AND s.input_revision = ?2 AND s.settled_at IS NOT NULL
                   AND (s.summarized_through IS NULL OR s.summarized_through < ?2)
                   AND s.failed_revision IS NOT ?2
                   AND {INCLUDED})"
                ),
                params![id.to_string(), revision],
                |row| row.get(0),
            )
            .map_err(failure)
        })
        .await
    }

    pub(crate) async fn expire_settlement(
        &self,
        after: Option<Uuid>,
        cutoff: Timestamp,
        now: Timestamp,
        limit: u32,
    ) -> Result<SettlementPage, SqliteError> {
        let limit = limit.clamp(1, SETTLEMENT_PAGE_SIZE);
        self.with_conn(move |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(failure)?;
            if after.is_none() {
                seed_recent(&tx, cutoff, limit)?;
            }
            let page = tx.prepare(&format!(
                "SELECT c.id, {INCLUDED} FROM ros_conversations c WHERE c.id > ?1 ORDER BY c.id LIMIT ?2"
            )).map_err(failure)?.query_map(params![after.unwrap_or(Uuid::nil()).to_string(), limit],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)))
                .map_err(failure)?.collect::<Result<Vec<_>, _>>().map_err(failure)?;
            let mut settled = 0;
            for (id, included) in &page {
                if !included {
                    continue;
                }
                let mut current = state(&tx, id)?;
                if current.is_none() {
                    seed(&tx, id, None)?;
                    current = state(&tx, id)?;
                }
                if let Some(mut current) = current
                    && current.settled_at.is_none() && current.last_input_at <= cutoff
                {
                    current.settle(now);
                    save(&tx, &current)?;
                    settled += 1;
                }
            }
            let next = page.last().map(|(id, _)| parse_id(id)).transpose()?;
            tx.commit().map_err(failure)?;
            Ok(SettlementPage { next, settled })
        }).await
    }
}

fn seed_recent(conn: &Connection, cutoff: Timestamp, limit: u32) -> Result<(), SqliteError> {
    let mut query = conn
        .prepare(&format!(
            "SELECT c.id FROM ros_conversations c
          WHERE c.id IN (
            WITH recent AS (
                SELECT conversation_id, seek_time FROM rh_transcript_times
                WHERE seek_time > ?1 ORDER BY seek_time DESC LIMIT ?2
            )
            SELECT conversation_id FROM (
                SELECT conversation_id, seek_time FROM recent
                UNION ALL
                SELECT conversation_id, rh_instant(created_at) FROM rh_transcript_times
                 WHERE seek_time IS NULL AND rh_instant(created_at) > ?1
            ) ORDER BY seek_time DESC LIMIT ?2
          )
            AND {INCLUDED}
            AND NOT EXISTS (SELECT 1 FROM rh_summary_state s WHERE s.conversation_id = c.id)"
        ))
        .map_err(failure)?;
    let ids = query
        .query_map(params![time::seek_key(cutoff), limit], |row| {
            row.get::<_, String>(0)
        })
        .map_err(failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)?;
    for id in ids {
        seed(conn, &id, None)?;
    }
    Ok(())
}

const fn failure(source: rusqlite::Error) -> SqliteError {
    SqliteError::Query {
        operation: "summary settlement",
        source,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RenderInput {
    id: String,
    tool_result: Option<String>,
}

pub(crate) fn rendered_inputs(
    conn: &Connection,
    id: &str,
) -> Result<Vec<RenderInput>, SqliteError> {
    use crate::message::{stored_role, transcript_metadata};
    use kernel::memory::MemoryBrowseOrder;
    use kernel::memory::summary_order::{MessagePosition, order_messages};
    use wire::{JsonObject, message::Role};

    struct Input {
        render: Option<RenderInput>,
        role: Role,
        metadata: Option<JsonObject>,
        created_at: Timestamp,
    }

    let mut statement = conn
        .prepare(
            "SELECT id, role, metadata, created_at, content <> '' OR coalesce(tool_name <> '', 0),
                CASE WHEN tool_name <> '' THEN nullif(tool_result, '') END
           FROM ros_messages WHERE conversation_id = ?1 ORDER BY rh_instant(created_at), id",
        )
        .map_err(failure)?;
    let inputs = statement
        .query_map([id], |row| {
            let render = if row.get(4)? {
                Some(RenderInput {
                    id: row.get(0)?,
                    tool_result: row.get(5)?,
                })
            } else {
                None
            };
            Ok((
                render,
                stored_role(row, 1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(failure)?
        .map(|row| {
            let (render, role, metadata, at) = row.map_err(failure)?;
            Ok(Input {
                render,
                role,
                metadata: transcript_metadata(&metadata),
                created_at: time::stored(&at, "created_at")?,
            })
        })
        .collect::<Result<Vec<_>, SqliteError>>()?;
    Ok(order_messages(inputs, MemoryBrowseOrder::Asc, |input| {
        MessagePosition::new(input.created_at, input.role, input.metadata.as_ref())
    })
    .into_iter()
    .filter_map(|input| input.render)
    .collect())
}

pub(crate) fn capture_observation(
    key: &str,
    settings: Option<&wire::JsonObject>,
    finalize: Option<bool>,
) -> Option<SessionObservation> {
    match SessionObservation::from_capture(key, settings, finalize) {
        Ok(observation) => observation,
        Err(error) => {
            tracing::debug!(%error, "ignoring capture observation with an invalid timestamp");
            None
        }
    }
}
