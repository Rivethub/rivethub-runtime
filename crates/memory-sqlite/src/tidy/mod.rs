mod samples;
mod upkeep;

use jiff::Timestamp;
use std::collections::BTreeSet;
use std::str::FromStr;

use async_trait::async_trait;
use kernel::filing::conversation_keys;
use kernel::tidy::Batch;
use kernel::tidy::BatchId;
use kernel::tidy::EditRecord;
use kernel::tidy::Evidence;
use kernel::tidy::Fingerprint;
use kernel::tidy::Proposal;
use kernel::tidy::ProposalId;
use kernel::tidy::ProposalState;
use kernel::tidy::Proposer;
use kernel::tidy::Reason;
use kernel::tidy::RecentFiling;
use kernel::tidy::Suggested;
use kernel::tidy::TidyError;
use kernel::tidy::TidyInputs;
use kernel::tidy::TidyStore;
use kernel::tidy::TreeEdit;
use kernel::topics::TopicId;
use kernel::topics::TopicName;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use uuid::Uuid;
use wire::session_id::SessionId;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time::stamp;

const PROPOSAL_COLUMNS: &str = "p.id, p.batch_id, b.proposed_by, b.proposed_at, p.change, p.reason, \
                                p.evidence, p.state, p.decided_at";

type ProposalColumns = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
);

impl SqliteStore {
    pub(crate) async fn tidy<T, F>(&self, operation: &'static str, work: F) -> Result<T, TidyError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, TidyError> + Send + 'static,
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
        .map_err(|error| TidyError::Unavailable(Box::new(error)))?
    }
}

pub(crate) fn failed(operation: &'static str) -> impl FnOnce(rusqlite::Error) -> TidyError {
    move |source| TidyError::Unavailable(Box::new(SqliteError::Query { operation, source }))
}

pub(crate) fn stored<E>(field: &'static str) -> impl FnOnce(E) -> TidyError
where
    E: std::error::Error + Send + Sync + 'static,
{
    move |source| {
        TidyError::Unavailable(Box::new(SqliteError::Invalid {
            message: format!("stored {field} is invalid: {source}"),
        }))
    }
}

pub(crate) fn count(value: i64) -> Result<u64, TidyError> {
    u64::try_from(value).map_err(stored("count"))
}

fn at(text: &str, column: &'static str) -> Result<Timestamp, TidyError> {
    crate::time::stored(text, column).map_err(|error| TidyError::Unavailable(Box::new(error)))
}

fn uuid(text: &str, field: &'static str) -> Result<Uuid, TidyError> {
    Uuid::parse_str(text).map_err(stored(field))
}

fn proposal(columns: ProposalColumns) -> Result<Proposal, TidyError> {
    let (id, batch, by, proposed_at, change, reason, evidence, state, decided_at) = columns;
    let change: EditRecord = serde_json::from_str(&change).map_err(stored("change"))?;
    let evidence: Evidence = serde_json::from_str(&evidence).map_err(stored("evidence"))?;
    Ok(Proposal {
        id: ProposalId::from_uuid(uuid(&id, "proposal id")?),
        batch: BatchId::from_uuid(uuid(&batch, "batch id")?),
        by: Proposer::from_str(&by).map_err(stored("proposed_by"))?,
        proposed_at: at(&proposed_at, "proposed_at")?,
        change: TreeEdit::try_from(change)?,
        reason: Reason::new(&reason)?,
        evidence,
        state: ProposalState::from_str(&state).map_err(stored("state"))?,
        decided_at: decided_at
            .as_deref()
            .map(|text| at(text, "decided_at"))
            .transpose()?,
    })
}

fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProposalColumns> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn proposals(
    conn: &Connection,
    filter: &str,
    value: Option<String>,
) -> Result<Vec<Proposal>, TidyError> {
    let sql = format!(
        "SELECT {PROPOSAL_COLUMNS} FROM rh_tidy_proposals p
           JOIN rh_tidy_batches b ON b.id = p.batch_id
          WHERE {filter}
          ORDER BY b.proposed_at DESC, b.id, p.position"
    );
    let mut statement = conn.prepare(&sql).map_err(failed("tidy proposals"))?;
    let rows = match value {
        Some(value) => statement.query_map(params![value], read),
        None => statement.query_map([], read),
    }
    .map_err(failed("tidy proposals"))?;
    let mut found = rows
        .map(|row| proposal(row.map_err(failed("tidy proposals"))?))
        .collect::<Result<Vec<_>, _>>()?;
    samples::current_titles(conn, &mut found)?;
    Ok(found)
}

fn batches(proposals: Vec<Proposal>) -> Vec<Batch> {
    let mut batches: Vec<Batch> = Vec::new();
    for proposal in proposals {
        match batches.last_mut() {
            Some(batch) if batch.id == proposal.batch => batch.proposals.push(proposal),
            Some(_) | None => batches.push(Batch {
                id: proposal.batch,
                by: proposal.by,
                at: proposal.proposed_at,
                proposals: vec![proposal],
            }),
        }
    }
    batches
}

fn by_id(conn: &Connection, id: ProposalId) -> Result<Option<Proposal>, TidyError> {
    Ok(proposals(conn, "p.id = ?1", Some(id.to_string()))?
        .into_iter()
        .next())
}

fn insert(conn: &Connection, batch: &Batch) -> Result<Vec<Proposal>, TidyError> {
    conn.execute(
        "INSERT INTO rh_tidy_batches (id, proposed_by, proposed_at) VALUES (?1, ?2, ?3)",
        params![batch.id.to_string(), batch.by.to_string(), stamp(batch.at)],
    )
    .map_err(failed("tidy batch"))?;
    let mut kept = Vec::new();
    for (position, proposal) in batch.proposals.iter().enumerate() {
        let change =
            serde_json::to_string(&EditRecord::from(&proposal.change)).map_err(stored("change"))?;
        let evidence = serde_json::to_string(&proposal.evidence).map_err(stored("evidence"))?;
        let inserted = conn
            .execute(
                "INSERT INTO rh_tidy_proposals
                     (id, batch_id, position, fingerprint, change, reason, evidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (fingerprint) WHERE state IN ('pending', 'rejected') DO NOTHING",
                params![
                    proposal.id.to_string(),
                    batch.id.to_string(),
                    i64::try_from(position).map_err(stored("position"))?,
                    proposal.change.fingerprint()?.as_str(),
                    change,
                    proposal.reason.as_str(),
                    evidence,
                ],
            )
            .map_err(failed("tidy proposal"))?;
        if inserted > 0 {
            kept.push(proposal.clone());
        }
    }
    Ok(kept)
}

fn suggestions(conn: &Connection) -> Result<Vec<Suggested>, TidyError> {
    let mut statement = conn
        .prepare(
            "SELECT s.session_id, s.name, s.parent_id
               FROM rh_session_suggestions s
              WHERE s.topic_id IS NULL
                AND NOT EXISTS (SELECT 1 FROM rh_session_placements p WHERE p.session_id = s.session_id)
              ORDER BY s.session_id",
        )
        .map_err(failed("tidy suggestions"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(failed("tidy suggestions"))?;
    let mut suggested = Vec::new();
    for row in rows {
        let (session, name, parent) = row.map_err(failed("tidy suggestions"))?;
        let session = SessionId::from_str(&session).map_err(stored("session_id"))?;
        suggested.push(Suggested {
            title: title(conn, &session)?,
            session,
            name: TopicName::new(&name)?,
            parent: parent
                .as_deref()
                .map(|parent| uuid(parent, "parent_id").map(TopicId::from_uuid))
                .transpose()?,
        });
    }
    Ok(suggested)
}

fn title(conn: &Connection, session: &SessionId) -> Result<Option<String>, TidyError> {
    let keys = serde_json::to_string(&conversation_keys(session)).map_err(stored("keys"))?;
    conn.query_row(
        "SELECT title FROM ros_conversations
          WHERE session_key IN (SELECT value FROM json_each(?1)) AND title IS NOT NULL
          ORDER BY updated_at DESC LIMIT 1",
        params![keys],
        |row| row.get(0),
    )
    .optional()
    .map_err(failed("tidy titles"))
}

fn recent(conn: &Connection, since: Timestamp) -> Result<Vec<RecentFiling>, TidyError> {
    let mut statement = conn
        .prepare(
            "SELECT topic_id, count(*) FROM (
                 SELECT topic_id FROM rh_session_placements WHERE placed_at >= ?1
                 UNION ALL
                 SELECT topic_id FROM rh_conversation_placements WHERE placed_at >= ?1)
              GROUP BY topic_id",
        )
        .map_err(failed("tidy recent filings"))?;
    let rows = statement
        .query_map(params![stamp(since)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(failed("tidy recent filings"))?;
    rows.map(|row| {
        let (topic, filed) = row.map_err(failed("tidy recent filings"))?;
        Ok(RecentFiling {
            topic: TopicId::from_uuid(uuid(&topic, "topic_id")?),
            count: count(filed)?,
        })
    })
    .collect()
}

#[async_trait]
impl TidyStore for SqliteStore {
    async fn propose(&self, batch: Batch) -> Result<Batch, TidyError> {
        self.tidy("tidy propose", move |conn| {
            let kept = insert(conn, &batch)?;
            Ok(Batch {
                proposals: kept,
                ..batch
            })
        })
        .await
    }

    async fn pending(&self) -> Result<Vec<Batch>, TidyError> {
        self.tidy("tidy pending", |conn| {
            Ok(batches(proposals(conn, "p.state = 'pending'", None)?))
        })
        .await
    }

    async fn proposal(&self, id: ProposalId) -> Result<Option<Proposal>, TidyError> {
        self.tidy("tidy proposal", move |conn| by_id(conn, id))
            .await
    }

    async fn batch(&self, id: BatchId) -> Result<Option<Batch>, TidyError> {
        self.tidy("tidy batch", move |conn| {
            Ok(
                batches(proposals(conn, "p.batch_id = ?1", Some(id.to_string()))?)
                    .into_iter()
                    .next(),
            )
        })
        .await
    }

    async fn decide(
        &self,
        id: ProposalId,
        state: ProposalState,
        decided: Timestamp,
    ) -> Result<Proposal, TidyError> {
        self.tidy("tidy decide", move |conn| {
            let changed = conn
                .execute(
                    "UPDATE rh_tidy_proposals SET state = ?2, decided_at = ?3
                      WHERE id = ?1 AND state = 'pending'",
                    params![id.to_string(), state.to_string(), stamp(decided)],
                )
                .map_err(failed("tidy decide"))?;
            let current = by_id(conn, id)?.ok_or(TidyError::UnknownProposal { id })?;
            if changed == 0 {
                return Err(TidyError::Decided {
                    id,
                    state: current.state,
                });
            }
            Ok(current)
        })
        .await
    }

    async fn last_batch(&self, by: Proposer) -> Result<Option<Timestamp>, TidyError> {
        self.tidy("tidy last batch", move |conn| {
            conn.query_row(
                "SELECT max(proposed_at) FROM rh_tidy_batches WHERE proposed_by = ?1",
                params![by.to_string()],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(failed("tidy last batch"))?
            .as_deref()
            .map(|text| at(text, "proposed_at"))
            .transpose()
        })
        .await
    }

    async fn known(&self) -> Result<BTreeSet<Fingerprint>, TidyError> {
        self.tidy("tidy fingerprints", |conn| {
            let mut statement = conn
                .prepare(
                    "SELECT fingerprint FROM rh_tidy_proposals WHERE state IN ('pending', 'rejected')",
                )
                .map_err(failed("tidy fingerprints"))?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(failed("tidy fingerprints"))?;
            rows.map(|row| Ok(Fingerprint::stored(row.map_err(failed("tidy fingerprints"))?)))
                .collect()
        })
        .await
    }

    async fn inputs(&self, since: Timestamp) -> Result<TidyInputs, TidyError> {
        self.tidy("tidy inputs", move |conn| {
            Ok(TidyInputs {
                suggestions: suggestions(conn)?,
                recent: recent(conn, since)?,
            })
        })
        .await
    }
}
