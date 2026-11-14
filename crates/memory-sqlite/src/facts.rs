use std::str::FromStr as _;

use async_trait::async_trait;
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
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params;
use uuid::Uuid;
use wire::capture::IMPORTED_SESSION_PREFIX;
use wire::session_id::SessionId;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

const REGISTRY_DEFAULT_OWNER: &str = "owner";

const CLAIMS_SQL: &str = "SELECT (SELECT g.id FROM rh_entity_facts g
                   WHERE g.topic_id = f.topic_id AND g.key = f.key
                   ORDER BY g.created_at, g.id LIMIT 1),
            f.key, c.value, c.actor, c.pinned, c.session_id, c.summary_id, c.evidence,
            c.at, c.recorded_at, c.owner_user_id
       FROM rh_entity_fact_claims c
       JOIN rh_entity_facts f ON f.id = c.fact_id
      WHERE f.topic_id = ?1
      ORDER BY c.at, c.recorded_at, c.id";

const LINKS_SQL: &str = "SELECT DISTINCT f.topic_id, c.session_id
       FROM rh_entity_fact_claims c
       JOIN rh_entity_facts f ON f.id = c.fact_id
      WHERE c.session_id IS NOT NULL
      ORDER BY f.topic_id, c.session_id";

const SESSION_SQL: &str = "SELECT (SELECT c.title FROM ros_conversations c
                   WHERE c.session_key IN (SELECT value FROM json_each(?1)) AND c.title IS NOT NULL
                   ORDER BY c.updated_at DESC LIMIT 1),
            min(c.created_at), max(c.updated_at),
            (SELECT count(*) FROM ros_messages m
               JOIN ros_conversations k ON k.id = m.conversation_id
              WHERE k.session_key IN (SELECT value FROM json_each(?1))),
            (SELECT i.settings FROM ros_conversations i
              WHERE i.session_key IN (SELECT value FROM json_each(?1))
                AND substr(i.session_key, 1, length(?2)) = ?2
              ORDER BY i.updated_at DESC LIMIT 1)
       FROM ros_conversations c
      WHERE c.session_key IN (SELECT value FROM json_each(?1))";

pub(crate) struct ClaimRow {
    fact: String,
    key: String,
    value: String,
    actor: String,
    pinned: bool,
    session: Option<String>,
    summary: Option<String>,
    evidence: Option<String>,
    at: String,
    recorded_at: String,
    owner: String,
}

pub(crate) fn read_claim(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClaimRow> {
    Ok(ClaimRow {
        fact: row.get(0)?,
        key: row.get(1)?,
        value: row.get(2)?,
        actor: row.get(3)?,
        pinned: row.get(4)?,
        session: row.get(5)?,
        summary: row.get(6)?,
        evidence: row.get(7)?,
        at: row.get(8)?,
        recorded_at: row.get(9)?,
        owner: row.get(10)?,
    })
}

fn unavailable(error: impl std::error::Error + Send + Sync + 'static) -> FactError {
    FactError::Unavailable(Box::new(error))
}

fn failed(operation: &'static str) -> impl FnOnce(rusqlite::Error) -> FactError {
    move |source| unavailable(SqliteError::Query { operation, source })
}

fn invalid(field: &str, error: impl std::fmt::Display) -> FactError {
    unavailable(SqliteError::Invalid {
        message: format!("stored {field} is invalid: {error}"),
    })
}

fn said(row: &ClaimRow) -> Result<Said, FactError> {
    match (row.actor.as_str(), &row.session, &row.summary) {
        ("you", None, None) => Ok(Said::You {
            by: Owner::new(row.owner.clone()).map_err(|error| invalid("fact owner", error))?,
            pinned: row.pinned,
        }),
        ("wiki-writer", Some(session), Some(summary)) => Ok(Said::Writer {
            session: SessionId::from_str(session)
                .map_err(|error| invalid("fact session", error))?,
            summary: Uuid::parse_str(summary).map_err(|error| invalid("fact summary", error))?,
            evidence: row.evidence.as_deref().and_then(Evidence::quote),
            settled: false,
        }),
        (actor, _, _) => Err(invalid("fact actor", actor)),
    }
}

pub(crate) fn claim(row: &ClaimRow) -> Result<Claim, FactError> {
    Ok(Claim {
        fact: FactId::from_str(&row.fact).map_err(|error| invalid("fact id", error))?,
        key: FactKey::new(&row.key)?,
        value: FactValue::new(&row.value)?,
        said: said(row)?,
        at: time::stored(&row.at, "at").map_err(unavailable)?,
        recorded_at: time::stored(&row.recorded_at, "recorded_at").map_err(unavailable)?,
    })
}

fn fact_for(conn: &Connection, claim: &NewClaim) -> Result<String, FactError> {
    let entity = claim.entity.to_string();
    let found: Option<String> = conn
        .query_row(
            "SELECT id FROM rh_entity_facts WHERE topic_id = ?1 AND key = ?2
              ORDER BY created_at, id LIMIT 1",
            params![entity, claim.key.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(failed("fact lookup"))?;
    if let Some(id) = found {
        return Ok(id);
    }
    let id = FactId::random().to_string();
    conn.execute(
        "INSERT INTO rh_entity_facts (id, topic_id, key, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, entity, claim.key.as_str(), time::stamp(Utc::now())],
    )
    .map_err(failed("fact create"))?;
    Ok(id)
}

fn insert(conn: &Connection, claim: &NewClaim) -> Result<(), FactError> {
    let fact = fact_for(conn, claim)?;
    let (actor, pinned, session, summary, evidence, by) = match &claim.said {
        Said::You { by, pinned } => ("you", *pinned, None, None, None, Some(by.as_str())),
        Said::Writer {
            session,
            summary,
            evidence,
            settled: _,
        } => (
            "wiki-writer",
            false,
            Some(session.as_str()),
            Some(summary.to_string()),
            evidence.as_ref().map(Evidence::as_str),
            None,
        ),
    };
    conn.execute(
        "INSERT INTO rh_entity_fact_claims (id, fact_id, value, actor, pinned, session_id, summary_id,
                                            evidence, at, recorded_at, owner_user_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                 coalesce(?11, (SELECT k.owner_user_id FROM ros_summaries s
                                  JOIN ros_conversations k ON k.id = s.conversation_id
                                 WHERE s.id = ?7), ?12))
         ON CONFLICT (fact_id, summary_id) WHERE summary_id IS NOT NULL DO UPDATE
            SET value = excluded.value, evidence = excluded.evidence, at = excluded.at,
                recorded_at = excluded.recorded_at",
        params![
            Uuid::new_v4().to_string(),
            fact,
            claim.value.as_str(),
            actor,
            pinned,
            session,
            summary,
            evidence,
            time::stamp(claim.at),
            time::stamp(Utc::now()),
            by.or_else(|| claim.owner.as_ref().map(Owner::as_str)),
            REGISTRY_DEFAULT_OWNER,
        ],
    )
    .map_err(failed("fact claim"))?;
    Ok(())
}

fn session_row(conn: &Connection, session: &SessionId) -> Result<Option<FactSession>, FactError> {
    let keys = serde_json::to_string(&conversation_keys(session))
        .map_err(|error| invalid("session keys", error))?;
    let row: (
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        Option<String>,
    ) = conn
        .query_row(SESSION_SQL, params![keys, IMPORTED_SESSION_PREFIX], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .map_err(failed("fact sessions"))?;
    let (title, Some(started), Some(last), messages, imported) = row else {
        return Ok(None);
    };
    let provenance = imported
        .map(|settings| {
            serde_json::from_str::<wire::JsonObject>(&settings)
                .map(|settings| Provenance::from_settings(&settings))
                .map_err(|source| {
                    unavailable(SqliteError::StoredJson {
                        column: "settings",
                        source,
                    })
                })
        })
        .transpose()?;
    Ok(Some(FactSession {
        session: session.clone(),
        title,
        started_at: time::stored(&started, "created_at").map_err(unavailable)?,
        last_at: time::stored(&last, "updated_at").map_err(unavailable)?,
        messages: u64::try_from(messages)
            .map_err(|error| unavailable(SqliteError::Count(error)))?,
        provenance,
    }))
}

impl SqliteStore {
    async fn facts<T, F>(&self, operation: &'static str, work: F) -> Result<T, FactError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, FactError> + Send + 'static,
    {
        self.with_conn(move |conn| {
            let tx = conn.transaction().map_err(SqliteError::query(operation))?;
            let done = work(&tx);
            if done.is_ok() {
                tx.commit().map_err(SqliteError::query(operation))?;
            }
            Ok(done)
        })
        .await
        .map_err(unavailable)?
    }
}

#[async_trait]
impl FactStore for SqliteStore {
    async fn assert_facts(&self, claims: &[NewClaim]) -> Result<(), FactError> {
        let claims = claims.to_vec();
        self.facts("fact claim", move |conn| {
            claims.iter().try_for_each(|claim| insert(conn, claim))
        })
        .await
    }

    async fn claims(&self, entity: TopicId) -> Result<Vec<Claim>, FactError> {
        let entity = entity.to_string();
        self.facts("fact claims", move |conn| {
            let rows = conn
                .prepare(CLAIMS_SQL)
                .and_then(|mut statement| {
                    statement
                        .query_map(params![entity], read_claim)?
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .map_err(failed("fact claims"))?;
            rows.iter().map(claim).collect()
        })
        .await
    }

    async fn session_links(&self) -> Result<Vec<SessionLink>, FactError> {
        self.facts("fact links", move |conn| {
            let rows = conn
                .prepare(LINKS_SQL)
                .and_then(|mut statement| {
                    statement
                        .query_map([], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .map_err(failed("fact links"))?;
            rows.into_iter()
                .map(|(entity, session)| {
                    Ok(SessionLink {
                        entity: TopicId::from_str(&entity)
                            .map_err(|error| invalid("fact entity", error))?,
                        session: SessionId::from_str(&session)
                            .map_err(|error| invalid("fact session", error))?,
                    })
                })
                .collect()
        })
        .await
    }

    async fn fact_sessions(&self, sessions: &[SessionId]) -> Result<Vec<FactSession>, FactError> {
        let sessions = sessions.to_vec();
        self.facts("fact sessions", move |conn| {
            let mut found = Vec::new();
            for session in &sessions {
                found.extend(session_row(conn, session)?);
            }
            Ok(found)
        })
        .await
    }

    async fn entity_history(&self, _entity: TopicId) -> Result<Vec<EntityHistory>, FactError> {
        Ok(Vec::new())
    }
}
