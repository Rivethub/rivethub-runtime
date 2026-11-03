use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use axum::extract::{FromRequestParts, RawQuery, State, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use wire::events::{AgentEvent, AgentEventKind, SessionBroadcast, SessionList, StateResponse};

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::{Body, Query};
use crate::sessions::control::TerminalSignal;
use crate::tenancy::UserId;

#[derive(Debug, Deserialize)]
pub(crate) struct Selection {
    pub(crate) session: Option<String>,
}

fn requested_session(query: Option<&str>) -> Result<String, ApiError> {
    let Some(query) = query else {
        return Err(ApiError::NotFound("unknown session"));
    };
    let mut found = None;
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((key, value)) => (key, value),
            None => (pair, ""),
        };
        if form_text(key)? == "session" {
            found = Some(form_text(value)?);
            break;
        }
    }
    match found {
        Some(session) if !session.is_empty() => Ok(session),
        Some(_) | None => Err(ApiError::NotFound("unknown session")),
    }
}

pub(crate) fn form_text(raw: &str) -> Result<String, ApiError> {
    let spaced = raw.replace('+', " ");
    match percent_encoding::percent_decode_str(&spaced).decode_utf8() {
        Ok(text) => Ok(text.into_owned()),
        Err(error) => Err(ApiError::BadRequest(format!("invalid query: {error}"))),
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Accepted {
    pub ok: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ingested {
    pub ok: bool,
    pub ingested: usize,
}

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route("/event", post(event).fallback(super::not_found))
        .route("/events", post(events).fallback(super::not_found))
        .route("/sessions", get(list).fallback(super::not_found))
        .route("/state", get(state).fallback(super::not_found))
        .route("/session", delete(remove).fallback(super::not_found))
        .route("/ws", get(upgrade).fallback(super::not_found))
        .route("/api/events/event", post(event))
        .route("/api/events", post(events))
        .route("/api/events/sessions", get(list))
        .route("/api/events/state", get(state))
        .route("/api/events/ws", get(upgrade))
}

async fn event(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Body(event): Body<AgentEvent>,
) -> Result<Json<Accepted>, ApiError> {
    ingest(&gateway, &user, vec![event]).await?;
    Ok(Json(Accepted { ok: true }))
}

async fn events(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    request: axum::extract::Request,
) -> Result<Json<Ingested>, ApiError> {
    let bytes = crate::extract::limited_bytes(request, &gateway).await?;
    let events = crate::extract::decode_event_batch(&bytes)?;
    let ingested = events.len();
    ingest(&gateway, &user, events).await?;
    Ok(Json(Ingested { ok: true, ingested }))
}

async fn ingest(gateway: &Gateway, user: &UserId, events: Vec<AgentEvent>) -> Result<(), ApiError> {
    let signals: Vec<_> = events.iter().map(hook_signal).collect();
    if gateway.tenancy.local_user() == Some(user) {
        gateway.sessions.ingest_as_local(user, events)?;
    } else {
        gateway.sessions.ingest(user, events)?;
    }
    if let Some(terminal) = &gateway.terminal {
        for (session, signal) in signals {
            terminal.terminal_hook(user, &session, signal).await?;
        }
    }
    Ok(())
}

fn hook_signal(event: &AgentEvent) -> (String, TerminalSignal) {
    let signal = match &event.kind {
        AgentEventKind::TurnEnd { .. } => TerminalSignal::TurnEnded,
        AgentEventKind::ApprovalWait { tool, args, .. } => {
            TerminalSignal::Approval(wire::harness::TerminalApproval {
                tool: tool.clone(),
                input: args.clone(),
            })
        }
        AgentEventKind::SessionStart { .. }
        | AgentEventKind::SessionEnd { .. }
        | AgentEventKind::MessageUser { .. }
        | AgentEventKind::MessageAgent { .. }
        | AgentEventKind::ThinkingDelta { .. }
        | AgentEventKind::ToolStart { .. }
        | AgentEventKind::TaskPlan { .. }
        | AgentEventKind::TaskCheck { .. }
        | AgentEventKind::Activity { .. } => TerminalSignal::Progressed,
    };
    (event.session.clone(), signal)
}

async fn list(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
) -> Result<Json<SessionList>, ApiError> {
    let (snapshot, _) = gateway.sessions.subscribe(&user, None)?;
    Ok(Json(SessionList {
        sessions: snapshot.sessions,
    }))
}

async fn state(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    RawQuery(query): RawQuery,
) -> Result<Json<StateResponse>, ApiError> {
    let session = requested_session(query.as_deref())?;
    Ok(Json(gateway.sessions.state(&user, &session)?))
}

async fn remove(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    RawQuery(query): RawQuery,
) -> Result<Json<Accepted>, ApiError> {
    let session = requested_session(query.as_deref())?;
    gateway.sessions.authorize(&user, &session)?;
    if let Some(terminal) = &gateway.terminal {
        terminal.remove_session(&user, &session).await?;
    }
    gateway.sessions.remove(&user, &session)?;
    Ok(Json(Accepted { ok: true }))
}

async fn upgrade(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Query(query): Query<Selection>,
    request: axum::extract::Request,
) -> Response {
    let (mut parts, body) = request.into_parts();
    drop(body);
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(rejection) => {
            tracing::debug!(%rejection, "session upgrade rejected");
            return super::plain_get(&gateway, &parts.method, parts.uri.path()).await;
        }
    };
    let subscription = gateway.sessions.subscribe(&user, query.session.as_deref());
    let filter = query
        .session
        .as_deref()
        .map(crate::sessions::session_key)
        .transpose();
    upgrade
        .on_upgrade(move |socket| async move {
            let result = match (subscription, filter) {
                (Ok((snapshot, events)), Ok(filter)) => {
                    stream(socket, gateway, snapshot, events, filter).await
                }
                (Err(error), _) | (_, Err(error)) => refuse(socket, error).await,
            };
            if let Err(error) = result {
                tracing::debug!(%error, "session websocket ended");
            }
        })
        .into_response()
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SocketError {
    #[error(transparent)]
    Socket(#[from] axum::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("session event stream fell behind; reconnect for a snapshot")]
    Lagged,
}

async fn refuse(mut socket: WebSocket, error: ApiError) -> Result<(), SocketError> {
    socket
        .send(Message::Close(Some(CloseFrame {
            code: 4403,
            reason: error.to_string().into(),
        })))
        .await?;
    Ok(())
}

pub(crate) async fn send<T: Serialize>(
    socket: &mut WebSocket,
    value: &T,
) -> Result<(), SocketError> {
    socket
        .send(Message::Text(serde_json::to_string(value)?.into()))
        .await?;
    Ok(())
}

async fn stream(
    mut socket: WebSocket,
    gateway: Arc<Gateway>,
    snapshot: wire::events::Snapshot,
    mut events: tokio::sync::broadcast::Receiver<SessionBroadcast>,
    filter: Option<String>,
) -> Result<(), SocketError> {
    send(&mut socket, &snapshot).await?;
    loop {
        tokio::select! {
            () = gateway.sessions.shutdown.cancelled() => {
                socket.send(Message::Close(None)).await?;
                return Ok(());
            }
            frame = socket.recv() => match frame {
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                Some(Err(error)) => return Err(error.into()),
                Some(Ok(Message::Ping(data))) => socket.send(Message::Pong(data)).await?,
                Some(Ok(Message::Text(_) | Message::Binary(_) | Message::Pong(_))) => {},
            },
            event = events.recv() => match event {
                Ok(event) => {
                    let session = match &event { SessionBroadcast::Event(event) => Some(&event.session), SessionBroadcast::Removed(event) => Some(&event.session), SessionBroadcast::HubState(_) | SessionBroadcast::PluginChanged(_) | SessionBroadcast::PluginViews(_) | SessionBroadcast::TopicChanged(_) => None, SessionBroadcast::Snapshot(_) => return Err(SocketError::Lagged) };
                    let wanted = match (filter.as_ref(), session) { (None, _) => true, (Some(filter), Some(session)) => filter == session, (Some(_), None) => false };
                    if wanted { send(&mut socket, &event).await?; }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    socket.send(Message::Close(Some(CloseFrame { code: 1013, reason: "reconnect for snapshot".into() }))).await?;
                    return Err(SocketError::Lagged);
                }
            }
        }
    }
}
