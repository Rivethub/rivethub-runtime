use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use kernel::harness::{SessionOption, TurnAttachment};
use kernel::topics::DeliveredVia;
use kernel::topics::StartupContext;
use kernel::topics::render;
use serde::Deserialize;
use wire::gateway::agents::UserTurnAttachment;
use wire::gateway::sessions::{
    SessionPostAccepted, SessionPostRequest, SessionSteerRequest, SessionSteerResponse,
};
use wire::harness::{
    ApprovalDecision, HarnessSessionCreateRequest, HarnessSessionSummary, SteerOutcome,
};

use super::sessions::Accepted;
use super::terminal::{require, service};
use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;
use crate::routes::agents::Failure;
use crate::tenancy::UserId;
use crate::term::ChatStart;
use crate::uploads::AttachedTurn;

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route("/api/harness-sessions", post(create).get(list))
        .route(
            "/term/harness-sessions/{session}/transcript",
            get(transcript),
        )
        .route(
            "/api/terminal/harness-sessions/{session}/transcript",
            get(transcript),
        )
        .route("/api/harness-sessions/{session}/turn", post(prompt))
        .route("/api/harness-sessions/{session}/steer", post(steer))
        .route("/api/harness-sessions/{session}/stop", post(stop))
        .route(
            "/api/harness-sessions/{session}/approvals/{request}",
            post(approve),
        )
        .route("/api/sessions/{session}/messages", post(prompt))
}

async fn list(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
) -> Result<Json<wire::harness::HarnessSessionList>, ApiError> {
    Ok(Json(require(&gateway)?.harness_sessions(&user)))
}

async fn transcript(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(session): Path<String>,
) -> Result<Json<wire::gateway::sessions::TranscriptResponse>, ApiError> {
    require(&gateway)?;
    gateway.sessions.authorize_visible(&user, &session)?;
    let reader = gateway
        .transcript
        .as_ref()
        .ok_or_else(|| ApiError::Unavailable("transcript reader unavailable".into()))?;
    let canonical = session
        .parse::<wire::session_id::SessionId>()
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let command = canonical.harness().to_string();
    let turns = reader
        .read(&canonical)
        .await?
        .into_iter()
        .map(|turn| wire::gateway::sessions::TranscriptTurn {
            role: turn.role,
            text: turn.content,
            ts: Some(turn.at),
        })
        .collect();
    Ok(Json(wire::gateway::sessions::TranscriptResponse {
        id: session,
        command,
        turns,
        context_window: None,
        compact_at: None,
        context_source: None,
    }))
}

async fn create(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Body(request): Body<HarnessSessionCreateRequest>,
) -> Result<Json<HarnessSessionSummary>, Failure> {
    let service = service(&gateway)?;
    let topic = match request.topic_id.as_deref() {
        Some(key) => Some(super::topics::resolve(gateway.topics()?.as_ref(), key).await?),
        None => None,
    };
    let context = match &topic {
        Some(topic) => super::topics::prepare(&gateway, topic, request.prompt.as_deref()).await,
        None => None,
    };
    let rendered = topic
        .as_ref()
        .zip(context.as_ref())
        .and_then(|(topic, context)| render(context, topic.name.as_str()));
    let agent = match request.agent_id {
        Some(id) => Some(crate::routes::agents::launch_preset(&gateway, id).await?),
        None => None,
    };
    let harness = request
        .harness
        .or_else(|| agent.as_ref().and_then(|agent| agent.harness))
        .ok_or_else(|| ApiError::BadRequest("harness is required".into()))?;
    let delivered = match (&rendered, service.adapter()) {
        (Some(_), Some(adapter)) => adapter.system_prompt(harness).delivered_via(),
        _ => DeliveredVia::None,
    };
    let start = ChatStart {
        harness,
        model: request.model,
        effort: None,
        cwd: request.cwd.map(std::path::PathBuf::from),
        inherited_cwd: None,
        agent,
        startup_context: rendered,
        topic: topic.as_ref().map(|topic| topic.id.to_string()),
        lineage: None,
    };
    let mut summary = service.start_chat(&user, start).await?;
    if let Some(topic) = topic {
        let session = summary.session_id.clone();
        summary.startup_context = super::topics::begin(
            &gateway,
            &service,
            &user,
            super::topics::Started {
                session: &session,
                owner: session.as_str(),
                topic: &topic,
                context: context.map(|context| StartupContext {
                    delivered_via: delivered,
                    ..context
                }),
            },
        )
        .await?;
    }
    Ok(Json(summary))
}

#[derive(Debug, Deserialize)]
struct Posted {
    wait: Option<String>,
}

async fn prompt(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(session): Path<String>,
    Query(posted): Query<Posted>,
    Body(request): Body<SessionPostRequest>,
) -> Result<Response, Failure> {
    if request
        .user_id
        .as_ref()
        .is_some_and(|id| id != user.as_str())
    {
        return Err(ApiError::SessionForbidden.into());
    }
    if posted.wait.is_some() {
        return Err(ApiError::BadRequest(
            "wait is no longer offered here; delegate work with POST /api/tasks".into(),
        )
        .into());
    }
    Ok(accept(gateway, user, session, request).await?)
}

async fn accept(
    gateway: Arc<Gateway>,
    user: UserId,
    session: String,
    request: SessionPostRequest,
) -> Result<Response, ApiError> {
    if let Ok(canonical) = session.parse::<wire::session_id::SessionId>() {
        crate::history::refuse_elsewhere(&gateway, &canonical).await?;
    }
    let attachments = staged(&gateway, &user, request.attachments).await?;
    let chat = service(&gateway)?.chat(&user, &session)?;
    let options = SessionOption::for_turn(request.model, request.thinking);
    if !options.is_empty() {
        admit(&gateway, &session, &options)?;
    }
    if !attachments.is_empty() {
        let sent = crate::sessions::control::with_system_prompt(
            request.system_prompt.as_deref(),
            request.text.clone(),
        );
        journal(&gateway, &session, &sent, &attachments).await?;
    }
    chat.prompt(request.text, request.system_prompt, attachments, options)
        .await?;
    Ok(Json(SessionPostAccepted {
        accepted: true,
        session,
    })
    .into_response())
}

async fn journal(
    gateway: &Gateway,
    session: &str,
    text: &str,
    attachments: &[TurnAttachment],
) -> Result<(), ApiError> {
    let canonical = session
        .parse::<wire::session_id::SessionId>()
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    super::harnesses::open_store(gateway)
        .await?
        .record_turn(&canonical, AttachedTurn::new(text, attachments))
        .await
        .map_err(super::harnesses::upload_error)
}

async fn steer(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(session): Path<String>,
    Body(request): Body<SessionSteerRequest>,
) -> Result<Json<SessionSteerResponse>, ApiError> {
    let attachments = staged(&gateway, &user, request.attachments).await?;
    let chat = service(&gateway)?.chat(&user, &session)?;
    let outcome = chat
        .steer(request.text.clone(), attachments.clone())
        .await?;
    if outcome != SteerOutcome::PromptRequired && !attachments.is_empty() {
        journal(&gateway, &session, &request.text, &attachments).await?;
    }
    Ok(Json(SessionSteerResponse { outcome }))
}

fn admit(gateway: &Gateway, session: &str, options: &[SessionOption]) -> Result<(), ApiError> {
    let canonical = session
        .parse::<wire::session_id::SessionId>()
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let adapter = service(gateway)?
        .adapter()
        .ok_or_else(|| ApiError::Unavailable("chat harness unavailable".into()))?;
    for option in options {
        adapter.admit(&canonical, option)?;
    }
    Ok(())
}

async fn staged(
    gateway: &Gateway,
    user: &UserId,
    attachments: Option<Vec<UserTurnAttachment>>,
) -> Result<Vec<TurnAttachment>, ApiError> {
    let Some(attachments) = attachments.filter(|list| !list.is_empty()) else {
        return Ok(Vec::new());
    };
    let store = super::harnesses::open_store(gateway).await?;
    let now = Utc::now();
    let mut staged = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let found = store
            .attachment(user, &attachment.path_or_uri, now)
            .await
            .map_err(super::harnesses::upload_error)?;
        staged.push(found);
    }
    Ok(staged)
}

async fn stop(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(session): Path<String>,
) -> Result<Json<Accepted>, ApiError> {
    service(&gateway)?.chat(&user, &session)?.stop().await?;
    Ok(Json(Accepted { ok: true }))
}

#[derive(Debug, Deserialize)]
struct Approval {
    decision: ApprovalDecision,
}

async fn approve(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path((segment, request)): Path<(String, String)>,
    Body(approval): Body<Approval>,
) -> Result<Json<Accepted>, ApiError> {
    let decoded = wire::session_id::decode_segment(&segment)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let wire::session_id::SessionRef::Canonical(session) = decoded.session else {
        return Err(ApiError::BadRequest("canonical session required".into()));
    };
    service(&gateway)?
        .answer_approval(&user, &session, &request, approval.decision)
        .await?;
    Ok(Json(Accepted { ok: true }))
}
