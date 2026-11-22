use std::collections::HashMap;

use kernel::facts::{Evidence, FactError, FactKey, FactValue, NewClaim, Said, facts_as_of};
use kernel::owner::Owner;
use kernel::topics::TopicId;
use kernel::wiki::entity_pass::{EntityStoreError, FactState, LiveFact, PreparedFact};
use rusqlite::{Connection, OptionalExtension as _, params};
use uuid::Uuid;
use wire::session_id::SessionId;

use super::{error, query};
use crate::facts;

pub(super) struct Current {
    pub(super) facts: Vec<LiveFact>,
    pub(super) fixed: Vec<String>,
}

pub(super) fn current(conn: &Connection, entity: Uuid) -> Result<Current, EntityStoreError> {
    let claims = facts::claims_on(conn, &entity.to_string()).map_err(error)?;
    let states = facts_as_of(&claims, None);
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
            status: FactState::Current,
        })
        .collect();
    Ok(Current { facts, fixed })
}

pub(super) fn assert_snapshot(
    conn: &Connection,
    entity: Uuid,
    snapshot: &[PreparedFact],
    evidence: &str,
    owner: &str,
) -> Result<(), EntityStoreError> {
    if snapshot.is_empty() {
        return Ok(());
    }
    let owner = Owner::new(owner.to_owned()).map_err(error)?;
    let evidence = Evidence::quote(evidence);
    let mut sessions = HashMap::new();
    for fact in snapshot {
        let key = match FactKey::new(&fact.attribute) {
            Ok(key) => key,
            Err(failure @ (FactError::EmptyKey | FactError::TooLong { .. })) => {
                tracing::info!(attribute = %fact.attribute, %failure, "skipped an invalid fact key");
                continue;
            }
            Err(
                failure @ (FactError::EmptyValue
                | FactError::SummaryReference(_)
                | FactError::Unavailable(_)),
            ) => {
                return Err(error(failure));
            }
        };
        let value: String = fact.value.trim().chars().take(400).collect();
        if value.is_empty() {
            continue;
        }
        let value = FactValue::new(&value).map_err(error)?;
        let session = match sessions.entry(fact.summary_id) {
            std::collections::hash_map::Entry::Occupied(found) => found.into_mut(),
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(session(conn, fact.summary_id)?)
            }
        };
        if same(conn, entity, &key, session, &value)? {
            continue;
        }
        facts::insert(
            conn,
            &NewClaim {
                entity: TopicId::from_uuid(entity),
                key,
                value,
                said: Said::Writer {
                    session: session.clone(),
                    summary: fact.summary_id,
                    evidence: evidence.clone(),
                    settled: true,
                },
                owner: Some(owner.clone()),
                at: fact.as_of,
            },
        )
        .map_err(error)?;
    }
    Ok(())
}

fn same(
    conn: &Connection,
    entity: Uuid,
    key: &FactKey,
    session: &SessionId,
    value: &FactValue,
) -> Result<bool, EntityStoreError> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM rh_entity_fact_claims c
         JOIN rh_entity_facts f ON f.id = c.fact_id
         WHERE f.topic_id = ?1 AND f.key = ?2 AND c.session_id = ?3 AND c.value = ?4)",
        params![
            entity.to_string(),
            key.as_str(),
            session.as_str(),
            value.as_str()
        ],
        |row| row.get(0),
    )
    .map_err(query)
}

fn session(conn: &Connection, summary: Uuid) -> Result<SessionId, EntityStoreError> {
    let key: Option<String> = conn.query_row(
        "SELECT c.session_key FROM ros_summaries s JOIN ros_conversations c ON c.id = s.conversation_id
         WHERE s.id = ?1", [summary.to_string()], |row| row.get(0),
    ).optional().map_err(query)?;
    let key = key.filter(|key| !key.is_empty()).ok_or_else(|| {
        error(std::io::Error::other(format!(
            "summary {summary} has no session"
        )))
    })?;
    key.parse().map_err(error)
}
