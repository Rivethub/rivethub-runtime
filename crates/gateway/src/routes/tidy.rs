use std::str::FromStr;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::extract::State;
use axum::routing;
use chrono::Utc;
use kernel::queue::WorkerRole;
use kernel::tidy::Batch;
use kernel::tidy::BatchId;
use kernel::tidy::BatchOutcome;
use kernel::tidy::Decision;
use kernel::tidy::EditRecord;
use kernel::tidy::Evidence;
use kernel::tidy::Proposal;
use kernel::tidy::ProposalId;
use kernel::tidy::ProposalState;
use kernel::tidy::Proposer;
use kernel::tidy::RoleActivity;
use kernel::tidy::TidyError;
use kernel::tidy::TidyPorts;
use kernel::tidy::TreeEdit;
use kernel::tidy::WEEK;
use wire::gateway::tidy::EntityTypeCount;
use wire::gateway::tidy::FilingUpkeep;
use wire::gateway::tidy::MemoryUpkeep;
use wire::gateway::tidy::ProposalState as ProposalStateBody;
use wire::gateway::tidy::Proposer as ProposerBody;
use wire::gateway::tidy::TidyBatch;
use wire::gateway::tidy::TidyBatchDecision;
use wire::gateway::tidy::TidyChange;
use wire::gateway::tidy::TidyEvidence;
use wire::gateway::tidy::TidyFailure;
use wire::gateway::tidy::TidyPending;
use wire::gateway::tidy::TidyProposal;
use wire::gateway::tidy::TidySample;
use wire::gateway::tidy::TidySkipped;
use wire::gateway::tidy::WikiUpkeep;
use wire::gateway::tidy::WorkerActivity;
use wire::gateway::tidy::WorkerUpkeep;

use crate::Gateway;
use crate::error::ApiError;
use crate::tenancy::UserId;

pub(super) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/api/tidy/proposals",
            routing::get(pending).fallback(super::method_not_allowed),
        )
        .route(
            "/api/tidy/proposals/{proposal}/approve",
            routing::post(approve).fallback(super::method_not_allowed),
        )
        .route(
            "/api/tidy/proposals/{proposal}/reject",
            routing::post(reject).fallback(super::method_not_allowed),
        )
        .route(
            "/api/tidy/batches/{batch}/approve",
            routing::post(approve_batch).fallback(super::method_not_allowed),
        )
        .route(
            "/api/tidy/batches/{batch}/reject",
            routing::post(reject_batch).fallback(super::method_not_allowed),
        )
}

fn error(error: TidyError) -> ApiError {
    match error {
        TidyError::UnknownProposal { .. } | TidyError::UnknownBatch { .. } => {
            ApiError::Missing(error.to_string())
        }
        TidyError::Decided { .. } | TidyError::Stale { .. } => {
            ApiError::Conflict(error.to_string())
        }
        TidyError::EmptyReason => ApiError::BadRequest(error.to_string()),
        TidyError::Topic(error) => super::topics::topic_error(error),
        TidyError::Invalid { .. } => {
            tracing::error!("tidy store holds an unreadable proposal: {error}");
            ApiError::Internal
        }
        TidyError::Unavailable(source) => {
            tracing::error!("tidy store failed: {source}");
            ApiError::Unavailable("tidy store unavailable".into())
        }
        TidyError::PagesUnavailable => ApiError::Unavailable(error.to_string()),
        TidyError::Wiki(source) => {
            tracing::error!("tidy wiki change failed: {source}");
            ApiError::Unavailable("wiki pages unavailable".into())
        }
    }
}

fn record(proposal: Proposal) -> TidyProposal {
    TidyProposal {
        id: proposal.id.to_string(),
        batch_id: proposal.batch.to_string(),
        change: change(&proposal.change),
        reason: proposal.reason.as_str().to_owned(),
        evidence: evidence(proposal.evidence),
        proposed_by: proposer(proposal.by),
        proposed_at: proposal.proposed_at,
        state: state(proposal.state),
        decided_at: proposal.decided_at,
    }
}

fn change(edit: &TreeEdit) -> TidyChange {
    match EditRecord::from(edit) {
        EditRecord::Merge { source, into } => TidyChange::Merge { source, into },
        EditRecord::Move { topic, parent } => TidyChange::Move { topic, parent },
        EditRecord::Rename { topic, name } => TidyChange::Rename { topic, name },
        EditRecord::Close { topic } => TidyChange::Close { topic },
        EditRecord::Delete { topic } => TidyChange::Delete { topic },
        EditRecord::CreateTopic {
            topic,
            name,
            parent,
            aliases,
            sessions,
            conversations,
        } => TidyChange::CreateTopic {
            topic,
            name,
            parent,
            aliases,
            sessions,
            conversations,
        },
        EditRecord::FileInto {
            topic,
            sessions,
            conversations,
        } => TidyChange::FileInto {
            topic,
            sessions,
            conversations,
        },
        EditRecord::MergePage { page, into } => TidyChange::MergePage { page, into },
        EditRecord::RemovePage { page } => TidyChange::RemovePage { page },
        EditRecord::AddType {
            topic,
            name,
            parent,
        } => TidyChange::AddType {
            topic,
            name,
            parent,
        },
    }
}

fn evidence(evidence: Evidence) -> TidyEvidence {
    TidyEvidence {
        sessions: evidence.sessions,
        samples: evidence
            .samples
            .into_iter()
            .map(|sample| TidySample {
                id: sample.id,
                title: sample.title,
                last_active: sample.last_active,
            })
            .collect(),
    }
}

const fn proposer(by: Proposer) -> ProposerBody {
    match by {
        Proposer::Categorizer => ProposerBody::Categorizer,
        Proposer::Backfill => ProposerBody::Backfill,
    }
}

const fn state(state: ProposalState) -> ProposalStateBody {
    match state {
        ProposalState::Pending => ProposalStateBody::Pending,
        ProposalState::Approved => ProposalStateBody::Approved,
        ProposalState::Rejected => ProposalStateBody::Rejected,
    }
}

fn batch(batch: Batch) -> TidyBatch {
    TidyBatch {
        batch_id: batch.id.to_string(),
        proposed_by: proposer(batch.by),
        proposed_at: batch.at,
        proposals: batch.proposals.into_iter().map(record).collect(),
    }
}

fn outcome(outcome: BatchOutcome) -> TidyBatchDecision {
    TidyBatchDecision {
        decided: outcome.decided.into_iter().map(record).collect(),
        skipped: outcome
            .skipped
            .into_iter()
            .map(|skipped| TidySkipped {
                proposal_id: skipped.proposal.id.to_string(),
                reason: skipped.reason,
            })
            .collect(),
        failed: outcome
            .failed
            .into_iter()
            .map(|(id, error)| TidyFailure {
                proposal_id: id.to_string(),
                error: error.to_string(),
            })
            .collect(),
    }
}

fn proposal_id(segment: &str) -> Result<ProposalId, ApiError> {
    ProposalId::from_str(segment)
        .map_err(|_| ApiError::BadRequest(format!("{segment} is not a proposal id")))
}

fn batch_id(segment: &str) -> Result<BatchId, ApiError> {
    BatchId::from_str(segment)
        .map_err(|_| ApiError::BadRequest(format!("{segment} is not a batch id")))
}

fn ports<'a>(gateway: &'a Gateway, user: &'a UserId) -> Result<TidyPorts<'a>, ApiError> {
    Ok(TidyPorts {
        tidy: gateway.tidy()?.as_ref(),
        topics: gateway.topics()?.as_ref(),
        filing: gateway.filing()?.as_ref(),
        pages: gateway.startup_memory().map(|(pages, _)| pages),
        owner: user.owner(),
    })
}

async fn pending(State(gateway): State<Arc<Gateway>>) -> Result<Json<TidyPending>, ApiError> {
    let batches = gateway.tidy()?.pending().await.map_err(error)?;
    Ok(Json(TidyPending {
        batches: batches.into_iter().map(batch).collect(),
    }))
}

async fn approve(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<Json<TidyProposal>, ApiError> {
    let id = proposal_id(&segment)?;
    let approved = kernel::tidy::approve(&ports(&gateway, &user)?, id, Utc::now())
        .await
        .map_err(error)?;
    Ok(Json(record(approved)))
}

async fn reject(
    State(gateway): State<Arc<Gateway>>,
    Path(segment): Path<String>,
) -> Result<Json<TidyProposal>, ApiError> {
    let id = proposal_id(&segment)?;
    let rejected = kernel::tidy::reject(gateway.tidy()?.as_ref(), id, Utc::now())
        .await
        .map_err(error)?;
    Ok(Json(record(rejected)))
}

async fn approve_batch(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<Json<TidyBatchDecision>, ApiError> {
    decide_batch(&gateway, &user, &segment, Decision::Approve).await
}

async fn reject_batch(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<Json<TidyBatchDecision>, ApiError> {
    decide_batch(&gateway, &user, &segment, Decision::Reject).await
}

async fn decide_batch(
    gateway: &Gateway,
    user: &UserId,
    segment: &str,
    decision: Decision,
) -> Result<Json<TidyBatchDecision>, ApiError> {
    let id = batch_id(segment)?;
    let decided = kernel::tidy::decide_batch(&ports(gateway, user)?, id, decision, Utc::now())
        .await
        .map_err(error)?;
    Ok(Json(outcome(decided)))
}

fn activity(role: RoleActivity) -> WorkerActivity {
    WorkerActivity {
        last_finished_at: role.last_finished,
        failing: role.failing,
    }
}

pub(super) async fn upkeep(
    State(gateway): State<Arc<Gateway>>,
) -> Result<Json<MemoryUpkeep>, ApiError> {
    let store = gateway.upkeep()?;
    let filing = store.filing(Utc::now() - WEEK).await.map_err(error)?;
    let wiki = store.wiki().await.map_err(error)?;
    let tasks = store.tasks().await.map_err(error)?;
    let pending = gateway.tidy()?.pending().await.map_err(error)?;
    let role = |role| activity(RoleActivity::of(role, &tasks));
    Ok(Json(MemoryUpkeep {
        filing: FilingUpkeep {
            categorizer: filing.categorizer,
            you: filing.you,
            uncategorized: filing.uncategorized,
            left_out: filing.left_out,
            corrections_last_week: filing.corrections,
        },
        wiki: WikiUpkeep {
            topic_pages: wiki.topic_pages,
            entity_types: wiki
                .types
                .into_iter()
                .map(|tally| EntityTypeCount {
                    type_id: tally.topic.to_string(),
                    name: tally.name.as_str().to_owned(),
                    entities: tally.entities,
                })
                .collect(),
            loose_pages: wiki.loose_pages,
        },
        workers: WorkerUpkeep {
            embedder: role(WorkerRole::Embedder),
            summarizer: role(WorkerRole::Summarizer),
            categorizer: role(WorkerRole::Categorizer),
            wiki_builder: role(WorkerRole::WikiBuilder),
        },
        pending_proposals: pending
            .iter()
            .map(|batch| batch.proposals.len())
            .sum::<usize>()
            .try_into()
            .map_err(|_| ApiError::Internal)?,
    }))
}
