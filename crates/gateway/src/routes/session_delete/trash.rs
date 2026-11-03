use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use kernel::memory::{
    EMPTY_SESSIONS_MAX, SessionActivity, Speaker, SpokenTurn, TrashReason, TrashSignals,
    trash_verdict,
};
use kernel::transcript::{HarnessTranscriptTurn, SessionMessageRole};
use kernel::transcript::{ListLimit, StoreSession};
use wire::gateway::session_delete::{SessionTrashCandidate, SessionTrashList, SessionTrashReason};
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use futures::StreamExt as _;

use super::{NOT_FOUND, deletions, hidden, known, live, resolved, segment_session, visible};
use crate::Gateway;
use crate::error::ApiError;
use crate::routes::terminal::require;
use crate::tenancy::UserId;

const TRANSCRIPT_READS: usize = 32;

struct Considered {
    row: StoreSession,
    session: SessionId,
    updated_at: DateTime<Utc>,
}

fn considered(row: StoreSession, now: DateTime<Utc>) -> Option<Considered> {
    let updated_at = DateTime::from_timestamp_millis(row.updated_at)?;
    let settled = row.updated_at > 0 && row.parent_session_id.is_none() && now >= updated_at;
    settled.then(|| Considered {
        session: SessionId::new(row.harness, &row.id),
        updated_at,
        row,
    })
}

async fn listed(gateway: &Gateway, roster: &[HarnessId]) -> Vec<StoreSession> {
    match gateway.session_lister() {
        Some(lister) => lister.list(roster, ListLimit::most()).await,
        None => Vec::new(),
    }
}

async fn described(
    gateway: &Gateway,
    roster: &[HarnessId],
    shown: &BTreeSet<SessionId>,
    empty: Vec<String>,
) -> Result<Vec<StoreSession>, ApiError> {
    let Some(lister) = gateway.session_lister() else {
        return Ok(Vec::new());
    };
    let mut wanted: BTreeMap<HarnessId, Vec<String>> = BTreeMap::new();
    for key in empty {
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "CONTRACT B1: a memory session key that is not a canonical session id is never a trash candidate (note 75)"
            )
        )]
        let Ok(session) = key.parse::<SessionId>() else {
            continue;
        };
        if !shown.contains(&session) && roster.contains(&session.harness()) {
            wanted
                .entry(session.harness())
                .or_default()
                .push(session.native().to_owned());
        }
    }
    let mut rows = Vec::new();
    for (harness, natives) in wanted {
        rows.extend(lister.describe_many(harness, &natives).await?);
    }
    Ok(rows)
}

pub(super) async fn trash(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
) -> Result<Json<SessionTrashList>, ApiError> {
    let deletions = deletions(&gateway)?;
    let roster = require(&gateway)?.roster();
    let now = Utc::now();
    let (mut rows, empty) = tokio::join!(
        listed(&gateway, &roster),
        deletions.empty_sessions(EMPTY_SESSIONS_MAX)
    );
    let shown: BTreeSet<SessionId> = rows
        .iter()
        .map(|row| SessionId::new(row.harness, &row.id))
        .collect();
    rows.extend(described(&gateway, &roster, &shown, empty?).await?);
    let rows: Vec<Considered> = crate::routes::history::visible_rows(&gateway, &user, rows)
        .into_iter()
        .filter_map(|row| considered(row, now))
        .filter(|row| !live(&gateway, &row.session))
        .collect();
    let keys: Vec<String> = rows
        .iter()
        .map(|row| row.session.as_str().to_owned())
        .collect();
    let (deleted, kept, activity) = tokio::join!(
        deletions.tombstones(&keys),
        deletions.kept(&keys),
        deletions.session_activity(&keys)
    );
    let (deleted, kept, activity) = (deleted?, kept?, activity?);
    let open: Vec<(Considered, SessionActivity)> = rows
        .into_iter()
        .filter(|row| !deleted.contains_key(row.session.as_str()))
        .filter(|row| !hidden(&kept, row.session.as_str(), Some(row.updated_at)))
        .filter_map(|row| {
            let memory = activity.get(row.session.as_str())?;
            let brief =
                !memory.kept() && memory.user_messages <= 1 && memory.assistant_messages <= 1;
            brief.then(|| (row, memory.clone()))
        })
        .collect();
    let candidates: Vec<SessionTrashCandidate> = futures::stream::iter(open)
        .map(|(row, memory)| judged(&gateway, row, memory, now))
        .buffered(TRANSCRIPT_READS)
        .filter_map(std::future::ready)
        .collect()
        .await;
    Ok(Json(SessionTrashList { candidates }))
}

pub(super) async fn keep(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<StatusCode, ApiError> {
    let session = segment_session(&segment)?;
    let deletions = deletions(&gateway)?;
    if !visible(&gateway, &user, &session).await? || !known(&gateway, deletions, &session).await? {
        return Err(ApiError::NotFound(NOT_FOUND));
    }
    deletions
        .keep_session(session.as_str(), Some(user.as_str()), Utc::now())
        .await?;
    tracing::info!(session = %session, "session kept out of the trash review");
    Ok(StatusCode::NO_CONTENT)
}

fn spoken(turn: &HarnessTranscriptTurn) -> SpokenTurn<'_> {
    SpokenTurn {
        speaker: match turn.role {
            SessionMessageRole::User => Speaker::User,
            SessionMessageRole::Assistant => Speaker::Assistant,
        },
        text: &turn.text,
        worked: turn.tools.as_ref().is_some_and(|tools| !tools.is_empty())
            || turn
                .attachments
                .as_ref()
                .is_some_and(|attachments| !attachments.is_empty()),
    }
}

const fn reason(reason: TrashReason) -> SessionTrashReason {
    match reason {
        TrashReason::Empty => SessionTrashReason::Empty,
        TrashReason::OneWordReply => SessionTrashReason::OneWordReply,
    }
}

async fn judged(
    gateway: &Gateway,
    row: Considered,
    memory: SessionActivity,
    now: DateTime<Utc>,
) -> Option<SessionTrashCandidate> {
    let read = gateway
        .history()
        .transcript(
            gateway,
            &resolved(&row.session),
            kernel::transcript::TranscriptWindow::Latest,
        )
        .await;
    let turns = match read {
        Ok(transcript) => transcript.read.turns,
        Err(error) => {
            tracing::debug!(session = %row.session, %error, "transcript unreadable; not a trash candidate");
            return None;
        }
    };
    let transcript: Vec<SpokenTurn<'_>> = turns.iter().map(spoken).collect();
    let title = (row.row.title != row.row.id).then_some(row.row.title.as_str());
    let verdict = trash_verdict(&TrashSignals {
        native: &row.row.id,
        title,
        updated_at: row.updated_at,
        now,
        activity: &memory,
        transcript: &transcript,
    })?;
    Some(SessionTrashCandidate {
        session: row.session.clone(),
        harness: row.row.harness,
        title: title.map(str::to_owned),
        updated_at: row.updated_at,
        reason: reason(verdict.reason),
        user_turns: verdict.user_turns,
        reply: verdict.reply,
    })
}
