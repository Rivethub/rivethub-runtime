use std::str::FromStr as _;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::facts::Claim;
use kernel::facts::EntityHistory;
use kernel::facts::Evidence;
use kernel::facts::FactError;
use kernel::facts::FactId;
use kernel::facts::FactKey;
use kernel::facts::FactSession;
use kernel::facts::FactStore;
use kernel::facts::FactValue;
use kernel::facts::NewClaim;
use kernel::facts::Said;
use kernel::facts::SessionLink;
use kernel::filing::conversation_keys;
use kernel::memory::Provenance;
use kernel::owner::Owner;
use kernel::topics::TopicId;
use sqlx::PgConnection;
use sqlx::types::Json;
use uuid::Uuid;
use wire::JsonObject;
use wire::capture::IMPORTED_SESSION_PREFIX;
use wire::session_id::SessionId;

use crate::PostgresMemory;
use crate::error::Error;

const CLAIMS_SQL: &str = "SELECT (SELECT g.id FROM rh_entity_facts g
                   WHERE g.topic_id = f.topic_id AND g.key = f.key
                   ORDER BY g.created_at, g.id LIMIT 1) AS fact,
            f.key, c.value, c.actor, c.pinned, c.session_id, c.summary_id, c.evidence,
            c.at, c.recorded_at, c.owner_user_id, c.settled
       FROM rh_entity_fact_claims c
       JOIN rh_entity_facts f ON f.id = c.fact_id
      WHERE f.topic_id = $1
      ORDER BY c.at, c.recorded_at, c.id";

const LINKS_SQL: &str = "SELECT DISTINCT f.topic_id, c.session_id
       FROM rh_entity_fact_claims c
       JOIN rh_entity_facts f ON f.id = c.fact_id
      WHERE c.session_id IS NOT NULL
      ORDER BY f.topic_id, c.session_id";

const SESSION_SQL: &str = "WITH conv AS (
         SELECT id, session_key, title, created_at, updated_at, settings
           FROM rh_conversations WHERE session_key = ANY($1)
     )
     SELECT (SELECT title FROM conv WHERE title IS NOT NULL ORDER BY updated_at DESC LIMIT 1) AS title,
            (SELECT min(created_at) FROM conv) AS started_at,
            (SELECT max(updated_at) FROM conv) AS last_at,
            (SELECT coalesce(sum(n.total), 0)::bigint FROM rh_message_counts n
              WHERE n.conversation_id IN (SELECT id FROM conv)) AS messages,
            (SELECT coalesce(settings, '{}'::jsonb) FROM conv WHERE starts_with(session_key, $2)
              ORDER BY updated_at DESC LIMIT 1) AS import_settings";

const HISTORY_SQL: &str = "SELECT h.sentence,
            coalesce(s.latest_at, (h.session_on::timestamp AT TIME ZONE 'UTC')) AS at,
            c.session_key
       FROM rh_entity_history h
       LEFT JOIN rh_summaries s ON s.id = h.summary_id
       LEFT JOIN rh_conversations c ON c.id = coalesce(h.conversation_id, s.conversation_id)
      WHERE h.entity_id = $1
      ORDER BY 2 DESC, h.summary_id DESC";

const FACT_SQL: &str = "WITH found AS (
         SELECT id FROM rh_entity_facts WHERE topic_id = $1 AND key = $2
          ORDER BY created_at, id LIMIT 1
     ), made AS (
         INSERT INTO rh_entity_facts (id, topic_id, key, created_at)
         SELECT $3, $1, $2, $4 WHERE NOT EXISTS (SELECT 1 FROM found)
         RETURNING id
     )
     SELECT id FROM found UNION ALL SELECT id FROM made";

const CLAIM_SQL: &str = "INSERT INTO rh_entity_fact_claims
            (id, fact_id, value, actor, pinned, session_id, summary_id, evidence, at, recorded_at,
             owner_user_id, settled)
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
             coalesce($11, (SELECT s.owner_user_id FROM rh_summaries s WHERE s.id = $7),
                      rh_scope_principal()),
             $12)
     ON CONFLICT (fact_id, summary_id) WHERE summary_id IS NOT NULL DO UPDATE
        SET value = EXCLUDED.value, evidence = EXCLUDED.evidence, at = EXCLUDED.at,
            recorded_at = EXCLUDED.recorded_at, settled = EXCLUDED.settled";

#[derive(sqlx::FromRow)]
pub(crate) struct ClaimRow {
    fact: Uuid,
    key: String,
    value: String,
    actor: String,
    pinned: bool,
    session_id: Option<String>,
    summary_id: Option<Uuid>,
    evidence: Option<String>,
    at: DateTime<Utc>,
    recorded_at: DateTime<Utc>,
    owner_user_id: String,
    settled: bool,
}

#[derive(sqlx::FromRow)]
struct HistoryRow {
    sentence: String,
    at: DateTime<Utc>,
    session_key: Option<String>,
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    title: Option<String>,
    started_at: Option<DateTime<Utc>>,
    last_at: Option<DateTime<Utc>>,
    messages: i64,
    import_settings: Option<Json<JsonObject>>,
}

fn failed(operation: &'static str) -> impl FnOnce(sqlx::Error) -> FactError {
    move |source| FactError::Unavailable(Box::new(Error::operation(operation, source)))
}

fn stored(
    field: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> FactError {
    FactError::Unavailable(Box::new(Error::StoredValue {
        field,
        source: Box::new(source),
    }))
}

fn said(row: &ClaimRow) -> Result<Said, FactError> {
    match (row.actor.as_str(), &row.session_id, row.summary_id) {
        ("you", None, None) => Ok(Said::You {
            by: Owner::new(row.owner_user_id.clone())
                .map_err(|error| stored("fact owner", error))?,
            pinned: row.pinned,
        }),
        ("wiki-writer", Some(session), Some(summary)) => Ok(Said::Writer {
            session: SessionId::from_str(session).map_err(|error| stored("fact session", error))?,
            summary,
            evidence: row.evidence.as_deref().and_then(Evidence::quote),
            settled: row.settled,
        }),
        _ => Err(FactError::Unavailable(Box::new(Error::StoredValue {
            field: "fact actor",
            source: format!("unknown claim actor {:?}", row.actor).into(),
        }))),
    }
}

pub(crate) async fn claims_on(
    conn: &mut PgConnection,
    entity: Uuid,
) -> Result<Vec<Claim>, FactError> {
    sqlx::query_as::<_, ClaimRow>(CLAIMS_SQL)
        .bind(entity)
        .fetch_all(&mut *conn)
        .await
        .map_err(failed("fact claims"))?
        .iter()
        .map(claim)
        .collect()
}

const TOPIC_CLAIMS_SQL: &str = "SELECT f.topic_id,
            (SELECT g.id FROM rh_entity_facts g
              WHERE g.topic_id = f.topic_id AND g.key = f.key
              ORDER BY g.created_at, g.id LIMIT 1) AS fact,
            f.key, c.value, c.actor, c.pinned, c.session_id, c.summary_id, c.evidence,
            c.at, c.recorded_at, c.owner_user_id, c.settled
       FROM rh_entity_fact_claims c
       JOIN rh_entity_facts f ON f.id = c.fact_id
      WHERE f.topic_id = ANY($1)
      ORDER BY f.topic_id, c.at, c.recorded_at, c.id";

#[derive(sqlx::FromRow)]
struct TopicClaimRow {
    topic_id: Uuid,
    #[sqlx(flatten)]
    claim: ClaimRow,
}

pub(crate) async fn claims_for<'e, E>(
    executor: E,
    topics: &[Uuid],
) -> Result<Vec<(Uuid, Claim)>, FactError>
where
    E: sqlx::PgExecutor<'e>,
{
    if topics.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, TopicClaimRow>(TOPIC_CLAIMS_SQL)
        .bind(topics)
        .fetch_all(executor)
        .await
        .map_err(failed("fact claims"))?;
    rows.into_iter()
        .map(|row| claim(&row.claim).map(|stored| (row.topic_id, stored)))
        .collect()
}

pub(crate) fn claim(row: &ClaimRow) -> Result<Claim, FactError> {
    Ok(Claim {
        fact: FactId::from_uuid(row.fact),
        key: FactKey::new(&row.key)?,
        value: FactValue::new(&row.value)?,
        said: said(row)?,
        at: row.at,
        recorded_at: row.recorded_at,
    })
}

pub(crate) async fn insert_claim(
    conn: &mut PgConnection,
    claim: &NewClaim,
    now: DateTime<Utc>,
) -> Result<(), FactError> {
    let fact: Uuid = sqlx::query_scalar(FACT_SQL)
        .bind(claim.entity.uuid())
        .bind(claim.key.as_str())
        .bind(FactId::random().uuid())
        .bind(now)
        .fetch_one(&mut *conn)
        .await
        .map_err(failed("fact create"))?;
    let (actor, pinned, session, summary, evidence, by, settled) = match &claim.said {
        Said::You { by, pinned } => ("you", *pinned, None, None, None, Some(by.as_str()), false),
        Said::Writer {
            session,
            summary,
            evidence,
            settled,
        } => (
            "wiki-writer",
            false,
            Some(session.as_str()),
            Some(*summary),
            evidence.as_ref().map(Evidence::as_str),
            None,
            *settled,
        ),
    };
    sqlx::query(CLAIM_SQL)
        .bind(Uuid::new_v4())
        .bind(fact)
        .bind(claim.value.as_str())
        .bind(actor)
        .bind(pinned)
        .bind(session)
        .bind(summary)
        .bind(evidence)
        .bind(claim.at)
        .bind(now)
        .bind(by.or_else(|| claim.owner.as_ref().map(Owner::as_str)))
        .bind(settled)
        .execute(&mut *conn)
        .await
        .map_err(failed("fact claim"))?;
    Ok(())
}

#[async_trait]
impl FactStore for PostgresMemory {
    async fn assert_facts(&self, claims: &[NewClaim]) -> Result<(), FactError> {
        let mut tx = self.pool.begin().await.map_err(failed("fact claim"))?;
        let now = Utc::now();
        for claim in claims {
            insert_claim(&mut tx, claim, now).await?;
        }
        tx.commit().await.map_err(failed("fact claim"))
    }

    async fn claims(&self, entity: TopicId) -> Result<Vec<Claim>, FactError> {
        sqlx::query_as::<_, ClaimRow>(CLAIMS_SQL)
            .bind(entity.uuid())
            .fetch_all(&self.pool)
            .await
            .map_err(failed("fact claims"))?
            .iter()
            .map(claim)
            .collect()
    }

    async fn session_links(&self) -> Result<Vec<SessionLink>, FactError> {
        sqlx::query_as::<_, (Uuid, String)>(LINKS_SQL)
            .fetch_all(&self.pool)
            .await
            .map_err(failed("fact links"))?
            .into_iter()
            .map(|(entity, session)| {
                Ok(SessionLink {
                    entity: TopicId::from_uuid(entity),
                    session: SessionId::from_str(&session)
                        .map_err(|error| stored("fact session", error))?,
                })
            })
            .collect()
    }

    async fn fact_sessions(&self, sessions: &[SessionId]) -> Result<Vec<FactSession>, FactError> {
        let mut found = Vec::new();
        for session in sessions {
            let row = sqlx::query_as::<_, SessionRow>(SESSION_SQL)
                .bind(conversation_keys(session))
                .bind(IMPORTED_SESSION_PREFIX)
                .fetch_one(&self.pool)
                .await
                .map_err(failed("fact sessions"))?;
            if let (Some(started_at), Some(last_at)) = (row.started_at, row.last_at) {
                found.push(FactSession {
                    session: session.clone(),
                    title: row.title,
                    started_at,
                    last_at,
                    messages: u64::try_from(row.messages)
                        .map_err(|error| stored("fact session messages", error))?,
                    provenance: row
                        .import_settings
                        .map(|settings| Provenance::from_settings(&settings.0)),
                });
            }
        }
        Ok(found)
    }

    async fn entity_history(&self, entity: TopicId) -> Result<Vec<EntityHistory>, FactError> {
        let rows = sqlx::query_as::<_, HistoryRow>(HISTORY_SQL)
            .bind(entity.uuid())
            .fetch_all(&self.pool)
            .await
            .map_err(failed("entity history"))?;
        rows.into_iter().map(history_line).collect()
    }
}

fn history_line(row: HistoryRow) -> Result<EntityHistory, FactError> {
    let session = match row.session_key.filter(|key| !key.is_empty()) {
        Some(key) => Some(
            SessionId::from_str(&key).map_err(|error| stored("entity history session", error))?,
        ),
        None => None,
    };
    Ok(EntityHistory {
        at: row.at,
        sentence: row.sentence,
        session,
    })
}
