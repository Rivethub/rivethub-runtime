use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{FromRequestParts, RawQuery, State, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use kernel::transcript::TranscriptWindow;
use tokio::sync::broadcast;
use tokio::time::{Instant, MissedTickBehavior};
use wire::gateway::sessions::HarnessTranscriptTurn;
use wire::gateway::sessions::{
    HarnessSocketMessage, HarnessSocketRefusal, HarnessSocketRefusalCode,
};
use wire::harness::{HarnessCode, HarnessEvent, RosterCommand, SessionDriver};

use super::first_param;
use crate::Gateway;
use crate::error::ApiError;
use crate::history::{Resolved, authorize, live_summary};
use crate::routes::sessions::{SocketError, send};
use crate::tenancy::UserId;

const HEARTBEAT: Duration = Duration::from_secs(30);
const SYNC_INTERVAL: Duration = Duration::from_secs(2);
const SEND_BUFFER: usize = 1024 * 1024;
const FOLLOW_FAST: Duration = Duration::from_secs(1);
const FOLLOW_BUSY: Duration = Duration::from_secs(2);
const FOLLOW_SLOW: Duration = Duration::from_secs(5);

pub(super) async fn upgrade(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    RawQuery(query): RawQuery,
    request: axum::extract::Request,
) -> Response {
    let segment = match first_param(query.as_deref(), "session") {
        Ok(Some(segment)) if !segment.is_empty() => segment,
        Ok(Some(_) | None) => {
            return ApiError::BadRequest("session is required".to_owned()).into_response();
        }
        Err(error) => return error.into_response(),
    };
    let (mut parts, body) = request.into_parts();
    drop(body);
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    upgrade
        .max_write_buffer_size(SEND_BUFFER)
        .on_upgrade(move |socket| async move {
            if let Err(error) = attach(socket, gateway, user, segment).await {
                tracing::debug!(%error, "harness session socket ended");
            }
        })
        .into_response()
}

async fn attach(
    mut socket: WebSocket,
    gateway: Arc<Gateway>,
    user: UserId,
    segment: String,
) -> Result<(), SocketError> {
    let resolved = match open(&gateway, &user, &segment).await {
        Ok(resolved) => resolved,
        Err(refusal) => {
            send(&mut socket, &refusal).await?;
            socket.send(Message::Close(None)).await?;
            return Ok(());
        }
    };
    let events = gateway
        .terminal()
        .map(|terminal| terminal.subscribe_harness(&resolved.native));
    let result = Attached {
        socket,
        gateway: &gateway,
        resolved: &resolved,
        rev: 0,
        last: None,
        window: TranscriptWindow::Latest,
    }
    .stream(events)
    .await;
    if let Some(terminal) = gateway.terminal() {
        terminal.release_harness(&resolved.native);
    }
    result
}

async fn open(
    gateway: &Gateway,
    user: &UserId,
    segment: &str,
) -> Result<Resolved, HarnessSocketRefusal> {
    let resolved = gateway
        .history()
        .resolve(gateway, segment)
        .await
        .map_err(|error| refusal(HarnessSocketRefusalCode::InvalidSessionId, &error))?;
    authorize(gateway, user, &resolved).await.map_err(|error| {
        let code = if matches!(error, ApiError::SessionForbidden) {
            HarnessSocketRefusalCode::Forbidden
        } else {
            HarnessSocketRefusalCode::InvalidSessionId
        };
        refusal(code, &error)
    })?;
    Ok(resolved)
}

fn refusal(code: HarnessSocketRefusalCode, error: &ApiError) -> HarnessSocketRefusal {
    HarnessSocketRefusal::Error {
        code,
        message: error.to_string(),
    }
}

struct Attached<'a> {
    socket: WebSocket,
    gateway: &'a Gateway,
    resolved: &'a Resolved,
    rev: u64,
    last: Option<Vec<HarnessTranscriptTurn>>,
    window: TranscriptWindow,
}

struct Follow {
    terminal: bool,
    busy: bool,
    gap: Duration,
    due: Instant,
}

impl Follow {
    fn new(driver: Option<&HarnessEvent>) -> Self {
        let mut follow = Self {
            terminal: false,
            busy: false,
            gap: FOLLOW_FAST,
            due: Instant::now() + FOLLOW_FAST,
        };
        if let Some(driver) = driver {
            follow.driver(driver);
        }
        follow
    }

    fn driver(&mut self, event: &HarnessEvent) -> bool {
        let HarnessEvent::Driver { driver, busy, .. } = event else {
            return false;
        };
        let terminal = *driver == Some(SessionDriver::Terminal);
        if terminal && (!self.terminal || (*busy && !self.busy)) {
            self.gap = FOLLOW_FAST;
            self.due = Instant::now() + FOLLOW_FAST;
        }
        let left = self.terminal && !terminal;
        self.terminal = terminal;
        self.busy = *busy;
        left
    }

    fn elsewhere(&mut self) {
        self.terminal = true;
        self.busy = true;
    }

    fn read(&mut self, changed: bool) {
        let slowest = if self.busy { FOLLOW_BUSY } else { FOLLOW_SLOW };
        self.gap = if changed {
            FOLLOW_FAST
        } else {
            (self.gap * 2).min(slowest)
        };
        self.due = Instant::now() + self.gap;
    }
}

async fn next_event(
    events: &mut Option<broadcast::Receiver<HarnessEvent>>,
) -> Result<HarnessEvent, broadcast::error::RecvError> {
    match events {
        Some(events) => events.recv().await,
        None => std::future::pending().await,
    }
}

impl Attached<'_> {
    async fn stream(
        mut self,
        mut events: Option<broadcast::Receiver<HarnessEvent>>,
    ) -> Result<(), SocketError> {
        let mut heartbeat = tokio::time::interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut awaiting_pong = false;
        let mut last_sync: Option<Instant> = None;
        let mut follow = Follow::new(
            self.gateway
                .terminal()
                .and_then(|terminal| terminal.held_driver(&self.resolved.native))
                .as_ref(),
        );
        if let Ok(Some(_)) =
            crate::history::running_elsewhere(self.gateway, &self.resolved.session).await
        {
            follow.elsewhere();
        }
        loop {
            tokio::select! {
                () = self.gateway.sessions().shutdown.cancelled() => {
                    self.socket.send(Message::Close(None)).await?;
                    return Ok(());
                }
                frame = self.socket.recv() => match frame {
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Err(error)) => return Err(error.into()),
                    Some(Ok(Message::Ping(data))) => self.socket.send(Message::Pong(data)).await?,
                    Some(Ok(Message::Pong(_))) => awaiting_pong = false,
                    Some(Ok(Message::Text(text))) => {
                        if serde_json::from_str::<HarnessSocketMessage>(&text).is_ok_and(|message| message == HarnessSocketMessage::Sync)
                            && last_sync.is_none_or(|at| at.elapsed() >= SYNC_INTERVAL)
                        {
                            last_sync = Some(Instant::now());
                            self.snapshot().await?;
                        }
                    }
                    Some(Ok(Message::Binary(_))) => {}
                },
                _ = heartbeat.tick() => {
                    if awaiting_pong {
                        return Ok(());
                    }
                    awaiting_pong = true;
                    self.socket.send(Message::Ping(Vec::new().into())).await?;
                }
                () = tokio::time::sleep_until(follow.due), if follow.terminal => {
                    let changed = self.read(true).await?;
                    follow.read(changed);
                }
                event = next_event(&mut events) => match event {
                    Ok(event) => {
                        let left = follow.driver(&event);
                        let committed = matches!(event, HarnessEvent::TurnComplete { .. });
                        send(&mut self.socket, &event).await?;
                        if committed {
                            self.snapshot().await?;
                        } else if left {
                            self.read(true).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => events = None,
                    Err(broadcast::error::RecvError::Lagged(_)) => return Err(SocketError::Lagged),
                },
            }
        }
    }

    async fn snapshot(&mut self) -> Result<(), SocketError> {
        self.read(false).await.map(|_| ())
    }

    async fn read(&mut self, follow: bool) -> Result<bool, SocketError> {
        let resolved = self.resolved;
        let read = match self
            .gateway
            .history()
            .transcript(self.gateway, resolved, self.window)
            .await
        {
            Ok(read) => read,
            Err(ApiError::Code(HarnessCode::CapabilityUnsupported, _)) => return Ok(false),
            Err(error) if follow => {
                tracing::debug!(%error, "a followed transcript could not be read");
                return Ok(false);
            }
            Err(error) => {
                let code = if let ApiError::Code(code, _) = &error {
                    *code
                } else {
                    HarnessCode::BadRequest
                };
                send(
                    &mut self.socket,
                    &HarnessEvent::Error {
                        session_id: resolved.session.clone(),
                        code: code.to_string(),
                        delivery_id: None,
                        message: error.to_string(),
                        retryable: None,
                    },
                )
                .await?;
                return Ok(false);
            }
        };
        if let Some(start) = read.read.earlier {
            self.window = TranscriptWindow::From(start);
        }
        let turns = crate::history::body::turns(read.read.turns);
        if follow && self.last.as_ref() == Some(&turns) {
            return Ok(false);
        }
        self.rev += 1;
        self.last = Some(turns.clone());
        let held = self.held();
        send(
            &mut self.socket,
            &HarnessEvent::Transcript {
                session_id: resolved.session.clone(),
                rev: self.rev,
                from: 0,
                total: turns.len() as u64,
                turns,
                command: RosterCommand::from(resolved.harness),
                truncated_before: read.read.earlier.map(|_| true),
                earlier: read.read.earlier,
                context_used: read.read.context,
                context_window: Some(read.stamp.window),
                compact_at: Some(read.stamp.compact_at),
                context_source: Some(read.stamp.source),
            },
        )
        .await?;
        for event in &held {
            send(&mut self.socket, event).await?;
        }
        Ok(true)
    }

    fn held(&self) -> Vec<HarnessEvent> {
        let native = &self.resolved.native;
        let Some(terminal) = self.gateway.terminal() else {
            return Vec::new();
        };
        let mut held = terminal.pending_approvals(native);
        held.extend(terminal.held_context(native));
        held.extend(terminal.held_driver(native));
        held.extend(self.waiting());
        held
    }

    fn waiting(&self) -> Option<HarnessEvent> {
        let live = live_summary(self.gateway, &self.resolved.native)?;
        (live.blocked == Some(true)).then(|| HarnessEvent::SessionUpdated {
            session_id: live.session_id,
            status: live.status,
            previous_session_id: None,
            supersedes: None,
            blocked: Some(true),
        })
    }
}
