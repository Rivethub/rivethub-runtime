use std::collections::HashMap;

use chrono::Utc;
use kernel::facts::Evidence;
use kernel::facts::FactKey;
use kernel::facts::FactValue;
use kernel::facts::NewClaim;
use kernel::facts::Said;
use kernel::facts::facts_as_of;
use kernel::owner::Owner;
use kernel::topics::TopicId;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::FactState as FactStatus;
use kernel::wiki::entity_pass::KnownFact;
use kernel::wiki::entity_pass::LiveFact;
use kernel::wiki::entity_pass::PreparedFact;
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::Transaction;
use uuid::Uuid;
use wire::session_id::SessionId;

use crate::facts;

pub(super) struct CurrentFacts {
    pub facts: Vec<LiveFact>,
    pub fixed: Vec<String>,
}

pub(super) async fn current_facts(
    conn: &mut PgConnection,
    entity: Uuid,
) -> Result<CurrentFacts, EntityStoreError> {
    let claims = facts::claims_on(conn, entity).await.map_err(fact_error)?;
    Ok(from_claims(&claims))
}

pub(super) async fn known_facts(
    pool: &sqlx::PgPool,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<KnownFact>>, EntityStoreError> {
    let rows = facts::claims_for(pool, ids).await.map_err(fact_error)?;
    let mut grouped: HashMap<Uuid, Vec<_>> = HashMap::new();
    for (topic, claim) in rows {
        grouped.entry(topic).or_default().push(claim);
    }
    let mut facts = HashMap::new();
    for (topic, claims) in grouped {
        let current = facts_as_of(&claims, None)
            .into_iter()
            .map(|state| KnownFact {
                attribute: state.key.as_str().to_owned(),
                value: state.value.as_str().to_owned(),
            })
            .collect();
        facts.insert(topic, current);
    }
    Ok(facts)
}

pub(super) async fn latest_values(
    pool: &sqlx::PgPool,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, String>, EntityStoreError> {
    let rows = facts::claims_for(pool, ids).await.map_err(fact_error)?;
    let mut grouped: HashMap<Uuid, Vec<_>> = HashMap::new();
    for (topic, claim) in rows {
        grouped.entry(topic).or_default().push(claim);
    }
    let mut latest = HashMap::new();
    for (topic, claims) in grouped {
        let mut states = facts_as_of(&claims, None);
        states.sort_by(|left, right| right.since.cmp(&left.since).then(left.key.cmp(&right.key)));
        if let Some(state) = states.into_iter().next() {
            latest.insert(topic, state.value.as_str().to_owned());
        }
    }
    Ok(latest)
}

pub(super) async fn assert_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    entity: Uuid,
    snapshot: &[PreparedFact],
    evidence: &str,
    owner: &str,
) -> Result<(), EntityStoreError> {
    if snapshot.is_empty() {
        return Ok(());
    }
    let quote = Evidence::quote(evidence);
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: an owner that does not parse is left off the claim"
        )
    )]
    let by = Owner::new(owner.to_owned()).ok();
    let now = Utc::now();
    let mut sessions: HashMap<Uuid, SessionId> = HashMap::new();
    for fact in snapshot {
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "PATTERNS FALLBACK: a key that does not fit FactKey is skipped"
            )
        )]
        let Ok(key) = FactKey::new(&fact.attribute) else {
            tracing::info!(attribute = %fact.attribute, "skipped a fact whose key does not fit");
            continue;
        };
        let Some(value) = clipped_value(&fact.value) else {
            continue;
        };
        let session = if let Some(session) = sessions.get(&fact.summary_id) {
            session.clone()
        } else {
            let session = session_for(tx, fact.summary_id).await?;
            sessions.insert(fact.summary_id, session.clone());
            session
        };
        if same_claim(tx, entity, key.as_str(), session.as_str(), value.as_str()).await? {
            continue;
        }
        let claim = NewClaim {
            entity: TopicId::from_uuid(entity),
            key,
            value,
            said: Said::Writer {
                session,
                summary: fact.summary_id,
                evidence: quote.clone(),
                settled: true,
            },
            owner: by.clone(),
            at: fact.as_of,
        };
        facts::insert_claim(&mut *tx, &claim, now)
            .await
            .map_err(fact_error)?;
    }
    Ok(())
}

fn from_claims(claims: &[kernel::facts::Claim]) -> CurrentFacts {
    let states = facts_as_of(claims, None);
    let fixed = states
        .iter()
        .filter(|state| state.pinned)
        .map(|state| state.key.as_str().to_owned())
        .collect();
    let facts = states
        .into_iter()
        .map(|state| LiveFact {
            attribute: state.key.as_str().to_owned(),
            value: state.value.as_str().to_owned(),
            as_of: state.since,
            status: FactStatus::Current,
        })
        .collect();
    CurrentFacts { facts, fixed }
}

async fn same_claim(
    conn: &mut PgConnection,
    entity: Uuid,
    key: &str,
    session: &str,
    value: &str,
) -> Result<bool, EntityStoreError> {
    let found: Option<i32> = sqlx::query_scalar(
        "SELECT 1
           FROM rh_entity_fact_claims c
           JOIN rh_entity_facts f ON f.id = c.fact_id
          WHERE f.topic_id = $1
            AND f.key = $2
            AND c.session_id = $3
            AND c.value = $4
          LIMIT 1",
    )
    .bind(entity)
    .bind(key)
    .bind(session)
    .bind(value)
    .fetch_optional(&mut *conn)
    .await
    .map_err(query_error)?;
    Ok(found.is_some())
}

async fn session_for(
    conn: &mut PgConnection,
    summary: Uuid,
) -> Result<SessionId, EntityStoreError> {
    let key: Option<String> = sqlx::query_scalar(
        "SELECT c.session_key
           FROM rh_summaries s
           JOIN rh_conversations c ON c.id = s.conversation_id
          WHERE s.id = $1",
    )
    .bind(summary)
    .fetch_optional(&mut *conn)
    .await
    .map_err(query_error)?;
    let Some(key) = key.filter(|key| !key.is_empty()) else {
        return Err(EntityStoreError::Query(Box::new(std::io::Error::other(
            format!("summary {summary} has no session"),
        ))));
    };
    key.parse::<SessionId>().map_err(|error| {
        EntityStoreError::Query(Box::new(std::io::Error::other(format!(
            "summary {summary} session is not a session id: {error}"
        ))))
    })
}

fn clipped_value(text: &str) -> Option<FactValue> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let clipped: String = trimmed.chars().take(400).collect();
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a value that does not fit FactValue is absent"
        )
    )]
    let value = FactValue::new(&clipped).ok();
    value
}

fn fact_error(error: kernel::facts::FactError) -> EntityStoreError {
    EntityStoreError::Query(Box::new(error))
}

fn query_error(error: sqlx::Error) -> EntityStoreError {
    EntityStoreError::Query(Box::new(error))
}
