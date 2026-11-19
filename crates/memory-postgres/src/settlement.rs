use jiff::Timestamp;
use kernel::memory::settlement::{
    SETTLEMENT_PAGE_SIZE, SettleResult, SettleStatus, SettlementPage, SummaryState,
};
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wire::capture::SessionObservation;

use crate::capture::CaptureWriter;
use crate::{Error, PostgresMemory};

const STATE: &str = "SELECT s.* FROM rh_summary_state s WHERE s.conversation_id = $1";

pub(crate) const INCLUDED: &str = "c.session_key NOT LIKE 'heartbeat:%'
    AND NOT EXISTS (SELECT 1 FROM rh_session_trash t WHERE t.session_key = c.session_key)
    AND NOT EXISTS (SELECT 1 FROM rh_session_tombstones t WHERE t.session_key = c.session_key)";

const SEED: &str = "INSERT INTO rh_summary_state
    (conversation_id, input_revision, last_input_at, unsettled_since, historical_only)
    SELECT c.id, 0, source.at, source.at, true
    FROM rh_conversations c
    CROSS JOIN LATERAL (
        SELECT coalesce(c.last_summary_input_at, (SELECT max(m.created_at) FROM rh_messages m WHERE m.conversation_id = c.id),
                        c.created_at) AS at
    ) source";

async fn seed(conn: &mut PgConnection, id: Uuid) -> Result<(), Error> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "{SEED} WHERE c.id = $1 ON CONFLICT DO NOTHING"
    )))
    .bind(id)
    .execute(conn)
    .await
    .map_err(failure)?;
    Ok(())
}

async fn state(
    conn: &mut PgConnection,
    id: Uuid,
    locked: bool,
) -> Result<Option<SummaryState>, Error> {
    let query = if locked {
        "SELECT s.* FROM rh_summary_state s WHERE s.conversation_id = $1 FOR UPDATE"
    } else {
        STATE
    };
    sqlx::query(query)
        .bind(id)
        .fetch_optional(conn)
        .await
        .map_err(failure)?
        .map(|row| {
            Ok(SummaryState {
                conversation_id: row.try_get("conversation_id")?,
                input_revision: row.try_get("input_revision")?,
                last_input_at: row
                    .try_get::<jiff_sqlx::Timestamp, _>("last_input_at")?
                    .to_jiff(),
                unsettled_since: row
                    .try_get::<Option<jiff_sqlx::Timestamp>, _>("unsettled_since")?
                    .map(jiff_sqlx::Timestamp::to_jiff),
                settled_at: row
                    .try_get::<Option<jiff_sqlx::Timestamp>, _>("settled_at")?
                    .map(jiff_sqlx::Timestamp::to_jiff),
                summarized_through: row.try_get("summarized_through")?,
                failed_revision: row.try_get("failed_revision")?,
                historical_only: row.try_get("historical_only")?,
                last_observed_event_at: row
                    .try_get::<Option<jiff_sqlx::Timestamp>, _>("last_observed_event_at")?
                    .map(jiff_sqlx::Timestamp::to_jiff),
                last_observed_end_at: row
                    .try_get::<Option<jiff_sqlx::Timestamp>, _>("last_observed_end_at")?
                    .map(jiff_sqlx::Timestamp::to_jiff),
                last_observed_end_key: row.try_get("last_observed_end_key")?,
            })
        })
        .transpose()
        .map_err(failure)
}

async fn save(conn: &mut PgConnection, state: &SummaryState) -> Result<(), Error> {
    sqlx::query(
        "UPDATE rh_summary_state SET input_revision = $2, last_input_at = $3,
            unsettled_since = $4, settled_at = $5, failed_revision = $6,
            historical_only = $7, last_observed_event_at = $8,
            last_observed_end_at = $9, last_observed_end_key = $10
          WHERE conversation_id = $1",
    )
    .bind(state.conversation_id)
    .bind(state.input_revision)
    .bind(crate::time::BindTime::bind_time(state.last_input_at))
    .bind(crate::time::BindTime::bind_time(state.unsettled_since))
    .bind(crate::time::BindTime::bind_time(state.settled_at))
    .bind(state.failed_revision)
    .bind(state.historical_only)
    .bind(crate::time::BindTime::bind_time(
        state.last_observed_event_at,
    ))
    .bind(crate::time::BindTime::bind_time(state.last_observed_end_at))
    .bind(state.last_observed_end_key.as_deref())
    .execute(conn)
    .await
    .map_err(failure)?;
    Ok(())
}

impl CaptureWriter<'_> {
    pub(crate) async fn existing_conversation(
        &mut self,
        key: &str,
        agent: &str,
        owner: &str,
    ) -> Result<Option<Uuid>, Error> {
        sqlx::query_scalar(
            "SELECT id FROM rh_conversations
            WHERE session_key = $1 AND agent = $2 AND owner_user_id = $3",
        )
        .bind(key)
        .bind(agent)
        .bind(owner)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(failure)
    }

    pub(crate) async fn prior_summary_observation(
        &mut self,
        key: &str,
        agent: &str,
    ) -> Result<Option<SessionObservation>, Error> {
        use sqlx::types::Json;
        use wire::JsonObject;

        let settings: Option<Option<Json<JsonObject>>> = sqlx::query_scalar(
            "SELECT CASE WHEN jsonb_typeof(c.settings) = 'object' THEN c.settings END
               FROM rh_conversations c
             LEFT JOIN rh_summary_state s ON s.conversation_id = c.id
             WHERE c.session_key = $1 AND c.agent = $2 AND s.last_observed_event_at IS NULL",
        )
        .bind(key)
        .bind(agent)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(failure)?;
        Ok(settings
            .flatten()
            .and_then(|settings| capture_observation(key, Some(&settings.0), None)))
    }

    pub(crate) async fn record_summary_input(
        &mut self,
        id: Uuid,
        changed: bool,
        imported: bool,
        observation: Option<&SessionObservation>,
        prior: Option<SessionObservation>,
    ) -> Result<(), Error> {
        let now = sqlx::query_scalar::<_, jiff_sqlx::Timestamp>("SELECT clock_timestamp()")
            .fetch_one(&mut *self.tx)
            .await
            .map_err(failure)?
            .to_jiff();
        if changed {
            sqlx::query("UPDATE rh_conversations SET last_summary_input_at = $2 WHERE id = $1")
                .bind(id)
                .bind(crate::time::BindTime::bind_time(now))
                .execute(&mut *self.tx)
                .await
                .map_err(failure)?;
        }
        let mut current = state(&mut self.tx, id, true).await?;
        if current.is_none() {
            if changed {
                sqlx::query("INSERT INTO rh_summary_state
                    (conversation_id, input_revision, last_input_at, unsettled_since, historical_only)
                    VALUES ($1, 0, $2, $2, true) ON CONFLICT DO NOTHING")
                    .bind(id).bind(crate::time::BindTime::bind_time(now)).execute(&mut *self.tx).await.map_err(failure)?;
            } else {
                seed(&mut self.tx, id).await?;
            }
            current = state(&mut self.tx, id, true).await?;
        }
        if let Some(mut current) = current {
            let seeded =
                current.record_capture(now, false, imported, prior.as_ref().map(|o| (o.at, None)));
            let updated = current.record_capture(
                now,
                changed,
                imported,
                observation.map(|o| (o.at, o.end_key.as_deref())),
            );
            if seeded || updated {
                save(&mut self.tx, &current).await?;
            }
        }
        Ok(())
    }
}

impl PostgresMemory {
    pub(crate) async fn mark_settled(
        &self,
        session: &str,
        owner: &str,
    ) -> Result<Option<SettleResult>, Error> {
        let mut tx = self.pool.begin().await.map_err(failure)?;
        let id = kernel::memory::settlement::session_uuid(session);
        let found: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM rh_conversations
             WHERE owner_user_id = $1 AND (id = $2 OR session_key = $3)
             ORDER BY id LIMIT 2 FOR UPDATE",
        )
        .bind(owner)
        .bind(id)
        .bind(session)
        .fetch_all(&mut *tx)
        .await
        .map_err(failure)?;
        let [id] = found.as_slice() else {
            return Ok(None);
        };
        let mut current = state(&mut tx, *id, true).await?;
        if current.is_none() {
            seed(&mut tx, *id).await?;
            current = state(&mut tx, *id, true).await?;
        }
        let Some(mut current) = current else {
            return Ok(None);
        };
        let now = sqlx::query_scalar::<_, jiff_sqlx::Timestamp>("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await
            .map_err(failure)?;
        let previously_failed = current.failed_revision.is_some();
        let result = current.manual_settle(now.to_jiff());
        if result.status == SettleStatus::Settled || previously_failed {
            save(&mut tx, &current).await?;
        }
        tx.commit().await.map_err(failure)?;
        Ok(Some(result))
    }

    pub(crate) async fn read_summary_state(&self, id: Uuid) -> Result<Option<SummaryState>, Error> {
        let mut conn = self.pool.acquire().await.map_err(failure)?;
        state(&mut conn, id, false).await
    }

    pub(crate) async fn eligible_summary(&self, id: Uuid, revision: i64) -> Result<bool, Error> {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT EXISTS (SELECT 1 FROM rh_summary_state s
                 JOIN rh_conversations c ON c.id = s.conversation_id
                 WHERE c.id = $1 AND s.input_revision = $2 AND s.settled_at IS NOT NULL
                   AND (s.summarized_through IS NULL OR s.summarized_through < $2)
                   AND s.failed_revision IS DISTINCT FROM $2
                   AND {INCLUDED})"
        )))
        .bind(id)
        .bind(revision)
        .fetch_one(&self.pool)
        .await
        .map_err(failure)
    }

    pub(crate) async fn expire_settlement(
        &self,
        after: Option<Uuid>,
        cutoff: Timestamp,
        now: Timestamp,
        limit: u32,
    ) -> Result<SettlementPage, Error> {
        let limit = limit.clamp(1, SETTLEMENT_PAGE_SIZE);
        if after.is_none() {
            let mut recent = self.pool.begin().await.map_err(failure)?;
            seed_recent(&mut recent, cutoff, limit).await?;
            recent.commit().await.map_err(failure)?;
        }
        let mut tx = self.pool.begin().await.map_err(failure)?;
        let page: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM rh_conversations WHERE id > $1 ORDER BY id LIMIT $2",
        )
        .bind(after.unwrap_or(Uuid::nil()))
        .bind(i64::from(limit))
        .fetch_all(&mut *tx)
        .await
        .map_err(failure)?;
        let candidates: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT c.id FROM rh_conversations c
             LEFT JOIN rh_summary_state s ON s.conversation_id = c.id
             WHERE c.id = ANY($1) AND {INCLUDED}
               AND (s.input_revision IS NULL OR (s.settled_at IS NULL AND s.last_input_at <= $2))
             ORDER BY c.id FOR UPDATE OF c"
        )))
        .bind(&page)
        .bind(crate::time::BindTime::bind_time(cutoff))
        .fetch_all(&mut *tx)
        .await
        .map_err(failure)?;
        seed_ids(&mut tx, &candidates).await?;
        let settled = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE rh_summary_state s SET settled_at = $3, unsettled_since = NULL
             FROM rh_conversations c WHERE s.conversation_id = c.id AND c.id = ANY($1)
               AND s.settled_at IS NULL AND s.last_input_at <= $2 AND {INCLUDED}"
        )))
        .bind(&candidates)
        .bind(crate::time::BindTime::bind_time(cutoff))
        .bind(crate::time::BindTime::bind_time(now))
        .execute(&mut *tx)
        .await
        .map_err(failure)?
        .rows_affected();
        tx.commit().await.map_err(failure)?;
        Ok(SettlementPage {
            next: page.last().copied(),
            settled,
        })
    }
}

async fn seed_ids(conn: &mut PgConnection, ids: &[Uuid]) -> Result<(), Error> {
    if !ids.is_empty() {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "{SEED} WHERE c.id = ANY($1) AND {INCLUDED}
                AND NOT EXISTS (SELECT 1 FROM rh_summary_state s WHERE s.conversation_id = c.id)
                ON CONFLICT DO NOTHING"
        )))
        .bind(ids)
        .execute(conn)
        .await
        .map_err(failure)?;
    }
    Ok(())
}

async fn seed_recent(conn: &mut PgConnection, cutoff: Timestamp, limit: u32) -> Result<(), Error> {
    let ids: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT c.id FROM rh_conversations c
          WHERE c.id IN (SELECT m.conversation_id FROM rh_messages m
                           WHERE m.created_at > $1 ORDER BY m.created_at DESC LIMIT $2)
            AND {INCLUDED}
            AND NOT EXISTS (SELECT 1 FROM rh_summary_state s WHERE s.conversation_id = c.id)
          ORDER BY c.id FOR UPDATE OF c"
    )))
    .bind(crate::time::BindTime::bind_time(cutoff))
    .bind(i64::from(limit))
    .fetch_all(&mut *conn)
    .await
    .map_err(failure)?;
    seed_ids(conn, &ids).await
}

fn failure(source: sqlx::Error) -> Error {
    Error::operation("summary settlement", source)
}

pub(crate) async fn rendered_order(conn: &mut PgConnection, id: Uuid) -> Result<Vec<Uuid>, Error> {
    use kernel::memory::MemoryBrowseOrder;
    use kernel::memory::summary_order::{MessagePosition, order_messages};
    use sqlx::types::Json;
    use wire::{JsonObject, message::Role};

    #[derive(sqlx::FromRow)]
    struct Input {
        id: Uuid,
        role: Json<Role>,
        metadata: Option<Json<JsonObject>>,
        #[sqlx(try_from = "jiff_sqlx::Timestamp")]
        created_at: Timestamp,
        rendered: bool,
    }

    let inputs: Vec<Input> = sqlx::query_as(
        "SELECT id, to_json(role) AS role, created_at,
                CASE WHEN jsonb_typeof(metadata) = 'object' THEN metadata END AS metadata,
                (content <> '' OR coalesce(tool_name <> '', false)) AS rendered
           FROM rh_messages WHERE conversation_id = $1 ORDER BY created_at, id",
    )
    .bind(id)
    .fetch_all(conn)
    .await
    .map_err(failure)?;
    Ok(order_messages(inputs, MemoryBrowseOrder::Asc, |input| {
        MessagePosition::new(
            input.created_at,
            input.role.0,
            input.metadata.as_ref().map(|json| &json.0),
        )
    })
    .into_iter()
    .filter(|input| input.rendered)
    .map(|input| input.id)
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
