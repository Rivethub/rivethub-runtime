mod samples;
mod upkeep;

use jiff::Timestamp;
use std::collections::BTreeMap;
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
use sqlx::types::Json;
use uuid::Uuid;
use wire::session_id::SessionId;

use crate::Error;
use crate::PostgresMemory;

struct ProposalRow {
    id: Uuid,
    batch_id: Uuid,
    proposed_by: String,
    proposed_at: Timestamp,
    change: Json<EditRecord>,
    reason: String,
    evidence: Json<Evidence>,
    state: String,
    decided_at: Option<Timestamp>,
}

struct SuggestionRow {
    session_id: String,
    name: String,
    parent_id: Option<Uuid>,
}

pub(crate) fn failed(operation: &'static str) -> impl FnOnce(sqlx::Error) -> TidyError {
    move |source| TidyError::Unavailable(Box::new(Error::operation(operation, source)))
}

pub(crate) fn stored(
    field: &'static str,
) -> impl FnOnce(Box<dyn std::error::Error + Send + Sync>) -> TidyError {
    move |source| TidyError::Unavailable(Box::new(Error::StoredValue { field, source }))
}

pub(crate) fn count(value: i64) -> Result<u64, TidyError> {
    u64::try_from(value).map_err(|source| TidyError::Unavailable(Box::new(Error::Integer(source))))
}

fn proposal(row: ProposalRow) -> Result<Proposal, TidyError> {
    Ok(Proposal {
        id: ProposalId::from_uuid(row.id),
        batch: BatchId::from_uuid(row.batch_id),
        by: Proposer::from_str(&row.proposed_by)
            .map_err(|error| stored("proposed_by")(Box::new(error)))?,
        proposed_at: row.proposed_at,
        change: TreeEdit::try_from(row.change.0)?,
        reason: Reason::new(&row.reason)?,
        evidence: row.evidence.0,
        state: ProposalState::from_str(&row.state)
            .map_err(|error| stored("state")(Box::new(error)))?,
        decided_at: row.decided_at,
    })
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

impl PostgresMemory {
    async fn proposals_where(
        &self,
        pending: bool,
        proposal: Option<Uuid>,
        batch: Option<Uuid>,
    ) -> Result<Vec<Proposal>, TidyError> {
        let mut proposals = sqlx::query_as!(
            ProposalRow,
            r#"SELECT p.id, p.batch_id, b.proposed_by, b.proposed_at AS "proposed_at: jiff_sqlx::Timestamp",
                      p.change AS "change: Json<EditRecord>", p.reason,
                      p.evidence AS "evidence: Json<Evidence>", p.state, p.decided_at AS "decided_at!: crate::time::OptionalTimestamp"
                 FROM rh_tidy_proposals p
                 JOIN rh_tidy_batches b ON b.id = p.batch_id
                WHERE ($1 = false OR p.state = 'pending')
                  AND ($2::uuid IS NULL OR p.id = $2)
                  AND ($3::uuid IS NULL OR p.batch_id = $3)
                ORDER BY b.proposed_at DESC, b.id, p.position"#,
            pending,
            proposal,
            batch
        )
        .fetch_all(&self.pool)
        .await
        .map_err(failed("tidy proposals"))?
        .into_iter()
        .map(self::proposal)
        .collect::<Result<Vec<_>, _>>()?;
        self.current_titles(&mut proposals).await?;
        Ok(proposals)
    }
}

#[async_trait]
impl TidyStore for PostgresMemory {
    async fn propose(&self, batch: Batch) -> Result<Batch, TidyError> {
        let mut tx = self.pool.begin().await.map_err(failed("tidy propose"))?;
        sqlx::query!(
            "INSERT INTO rh_tidy_batches (id, proposed_by, proposed_at) VALUES ($1, $2, $3)",
            batch.id.uuid(),
            batch.by.to_string(),
            crate::time::BindTime::bind_time(batch.at) as _
        )
        .execute(&mut *tx)
        .await
        .map_err(failed("tidy batch"))?;
        let mut kept = Vec::new();
        for (position, proposal) in batch.proposals.into_iter().enumerate() {
            let fingerprint = proposal.change.fingerprint()?;
            let inserted = sqlx::query_scalar!(
                "INSERT INTO rh_tidy_proposals
                     (id, batch_id, position, fingerprint, change, reason, evidence)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (fingerprint) WHERE state IN ('pending', 'rejected') DO NOTHING
                 RETURNING id",
                proposal.id.uuid(),
                batch.id.uuid(),
                i32::try_from(position)
                    .map_err(|source| TidyError::Unavailable(Box::new(Error::Integer(source))))?,
                fingerprint.as_str(),
                Json(EditRecord::from(&proposal.change)) as _,
                proposal.reason.as_str(),
                Json(&proposal.evidence) as _
            )
            .fetch_optional(&mut *tx)
            .await
            .map_err(failed("tidy proposal"))?;
            if inserted.is_some() {
                kept.push(proposal);
            }
        }
        tx.commit().await.map_err(failed("tidy propose"))?;
        Ok(Batch {
            proposals: kept,
            ..batch
        })
    }

    async fn pending(&self) -> Result<Vec<Batch>, TidyError> {
        Ok(batches(self.proposals_where(true, None, None).await?))
    }

    async fn proposal(&self, id: ProposalId) -> Result<Option<Proposal>, TidyError> {
        Ok(self
            .proposals_where(false, Some(id.uuid()), None)
            .await?
            .into_iter()
            .next())
    }

    async fn batch(&self, id: BatchId) -> Result<Option<Batch>, TidyError> {
        Ok(
            batches(self.proposals_where(false, None, Some(id.uuid())).await?)
                .into_iter()
                .next(),
        )
    }

    async fn decide(
        &self,
        id: ProposalId,
        state: ProposalState,
        at: Timestamp,
    ) -> Result<Proposal, TidyError> {
        let decided = sqlx::query_scalar!(
            "UPDATE rh_tidy_proposals SET state = $2, decided_at = $3
              WHERE id = $1 AND state = 'pending' RETURNING id",
            id.uuid(),
            state.to_string(),
            crate::time::BindTime::bind_time(at) as _
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(failed("tidy decide"))?;
        let current = self
            .proposal(id)
            .await?
            .ok_or(TidyError::UnknownProposal { id })?;
        match decided {
            Some(_) => Ok(current),
            None => Err(TidyError::Decided {
                id,
                state: current.state,
            }),
        }
    }

    async fn last_batch(&self, by: Proposer) -> Result<Option<Timestamp>, TidyError> {
        sqlx::query_scalar!(
            r#"SELECT max(proposed_at) AS "max!: crate::time::OptionalTimestamp" FROM rh_tidy_batches WHERE proposed_by = $1"#,
            by.to_string()
        )
        .fetch_one(&self.pool)
        .await
        .map_err(failed("tidy last batch"))
        .map(Option::from)
    }

    async fn known(&self) -> Result<BTreeSet<Fingerprint>, TidyError> {
        Ok(sqlx::query_scalar!(
            "SELECT fingerprint FROM rh_tidy_proposals WHERE state IN ('pending', 'rejected')"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(failed("tidy fingerprints"))?
        .into_iter()
        .map(Fingerprint::stored)
        .collect())
    }

    async fn inputs(&self, since: Timestamp) -> Result<TidyInputs, TidyError> {
        let rows = sqlx::query_as!(
            SuggestionRow,
            "SELECT s.session_id, s.name, s.parent_id
               FROM rh_session_suggestions s
              WHERE s.topic_id IS NULL
                AND NOT EXISTS (SELECT 1 FROM rh_session_placements p WHERE p.session_id = s.session_id)
              ORDER BY s.session_id"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(failed("tidy suggestions"))?;
        let sessions = rows
            .iter()
            .map(|row| {
                SessionId::from_str(&row.session_id)
                    .map_err(|error| stored("session_id")(Box::new(error)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let titles = self.titles(&sessions).await?;
        let suggestions = rows
            .into_iter()
            .zip(sessions)
            .map(|(row, session)| {
                Ok(Suggested {
                    title: titles.get(&session).cloned(),
                    session,
                    name: TopicName::new(&row.name)?,
                    parent: row.parent_id.map(TopicId::from_uuid),
                })
            })
            .collect::<Result<Vec<_>, TidyError>>()?;
        let recent = sqlx::query!(
            r#"SELECT topic_id AS "topic_id!", count(*) AS "count!"
                 FROM (SELECT topic_id FROM rh_session_placements WHERE placed_at >= $1
                       UNION ALL
                       SELECT topic_id FROM rh_conversation_placements WHERE placed_at >= $1) f
                GROUP BY topic_id"#,
            crate::time::BindTime::bind_time(since) as _
        )
        .fetch_all(&self.pool)
        .await
        .map_err(failed("tidy recent filings"))?
        .into_iter()
        .map(|row| {
            Ok(RecentFiling {
                topic: TopicId::from_uuid(row.topic_id),
                count: count(row.count)?,
            })
        })
        .collect::<Result<Vec<_>, TidyError>>()?;
        Ok(TidyInputs {
            suggestions,
            recent,
        })
    }
}

impl PostgresMemory {
    async fn titles(
        &self,
        sessions: &[SessionId],
    ) -> Result<BTreeMap<SessionId, String>, TidyError> {
        let mut owner: BTreeMap<String, SessionId> = BTreeMap::new();
        for session in sessions {
            for key in conversation_keys(session) {
                owner.insert(key, session.clone());
            }
        }
        let keys: Vec<String> = owner.keys().cloned().collect();
        let rows = sqlx::query!(
            r#"SELECT session_key, title AS "title!"
                 FROM rh_conversations
                WHERE session_key = ANY($1) AND title IS NOT NULL
                ORDER BY updated_at"#,
            &keys
        )
        .fetch_all(&self.pool)
        .await
        .map_err(failed("tidy titles"))?;
        Ok(rows
            .into_iter()
            .filter_map(|row| Some((owner.get(&row.session_key)?.clone(), row.title)))
            .collect())
    }
}
