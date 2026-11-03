use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use chrono::Utc;
use tokio::sync::broadcast;
use tokio::time::{Instant, MissedTickBehavior};
use uuid::Uuid;
use wire::gateway::notifications::TaskDoneNotification;
use wire::plugin::process::PluginNotification;

use super::sessions::{SocketError, send};
use crate::Gateway;
use crate::tasks::Tasks;
use crate::tenancy::UserId;

const HEARTBEAT: Duration = Duration::from_secs(30);
const SEND_BUFFER: usize = 256 * 1024;

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new().route("/api/notifications/ws", get(upgrade))
}

async fn upgrade(
    State(gateway): State<Arc<Gateway>>,
    _user: UserId,
    upgrade: WebSocketUpgrade,
) -> Response {
    let tasks = gateway.tasks().cloned();
    let finished = tasks.as_ref().map(|tasks| tasks.watch.changed());
    let plugins = gateway.plugins().map(|host| host.notifications());
    upgrade
        .max_write_buffer_size(SEND_BUFFER)
        .on_upgrade(move |socket| async move {
            let listener = Listener {
                socket,
                gateway: &gateway,
                tasks,
            };
            if let Err(error) = listener.stream(finished, plugins).await {
                tracing::debug!(%error, "notifications socket ended");
            }
        })
}

async fn next_notice(
    plugins: &mut Option<broadcast::Receiver<PluginNotification>>,
) -> Result<PluginNotification, broadcast::error::RecvError> {
    match plugins {
        Some(plugins) => plugins.recv().await,
        None => std::future::pending().await,
    }
}

async fn next_finished(
    finished: &mut Option<broadcast::Receiver<Uuid>>,
) -> Result<Uuid, broadcast::error::RecvError> {
    match finished {
        Some(finished) => finished.recv().await,
        None => std::future::pending().await,
    }
}

struct Listener<'a> {
    socket: WebSocket,
    gateway: &'a Gateway,
    tasks: Option<Arc<Tasks>>,
}

impl Listener<'_> {
    async fn stream(
        mut self,
        mut finished: Option<broadcast::Receiver<Uuid>>,
        mut plugins: Option<broadcast::Receiver<PluginNotification>>,
    ) -> Result<(), SocketError> {
        let mut heartbeat = tokio::time::interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut awaiting_pong = false;
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
                    Some(Ok(Message::Text(_) | Message::Binary(_))) => {}
                },
                _ = heartbeat.tick() => {
                    if awaiting_pong {
                        return Ok(());
                    }
                    awaiting_pong = true;
                    self.socket.send(Message::Ping(Vec::new().into())).await?;
                }
                notice = next_notice(&mut plugins) => match notice {
                    Ok(notice) => send(&mut self.socket, &notice).await?,
                    Err(broadcast::error::RecvError::Closed) => plugins = None,
                    Err(broadcast::error::RecvError::Lagged(_)) => return Err(SocketError::Lagged),
                },
                id = next_finished(&mut finished) => match id {
                    Ok(id) => self.task_done(id).await?,
                    Err(broadcast::error::RecvError::Closed) => finished = None,
                    Err(broadcast::error::RecvError::Lagged(_)) => return Err(SocketError::Lagged),
                },
            }
        }
    }

    async fn task_done(&mut self, id: Uuid) -> Result<(), SocketError> {
        let Some(tasks) = &self.tasks else {
            return Ok(());
        };
        let task = match tasks.store.load(id).await {
            Ok(task) => task,
            Err(error) => {
                tracing::warn!(%error, %id, "finished task could not be read");
                return Ok(());
            }
        };
        if !task.status.terminal() {
            return Ok(());
        }
        send(
            &mut self.socket,
            &TaskDoneNotification {
                task_id: id.to_string(),
                status: super::tasks::status_out(task.status),
                parent_session_id: task.parent.session,
                child_session_id: task.child,
                agent: Some(task.target.agent),
                error: task.error,
                ts: Utc::now(),
            },
        )
        .await
    }
}
