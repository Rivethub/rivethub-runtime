use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;

use crate::Gateway;
use crate::error::ApiError;
use crate::sessions::Notice;
use crate::tenancy::UserId;

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new().route("/api/events/ws", get(upgrade))
}

pub(crate) fn form_text(raw: &str) -> Result<String, ApiError> {
    let spaced = raw.replace('+', " ");
    match percent_encoding::percent_decode_str(&spaced).decode_utf8() {
        Ok(text) => Ok(text.into_owned()),
        Err(error) => Err(ApiError::BadRequest(format!("invalid query: {error}"))),
    }
}

async fn upgrade(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    upgrade: WebSocketUpgrade,
) -> Response {
    let notices = gateway.sessions.notices();
    upgrade.on_upgrade(move |socket| async move {
        if let Err(error) = stream(socket, gateway, user, notices).await {
            tracing::debug!(%error, "notice websocket ended");
        }
    })
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SocketError {
    #[error(transparent)]
    Socket(#[from] axum::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("the notice stream fell behind; reconnect")]
    Lagged,
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
    user: UserId,
    mut notices: tokio::sync::broadcast::Receiver<Notice>,
) -> Result<(), SocketError> {
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
                Some(Ok(Message::Text(_) | Message::Binary(_) | Message::Pong(_))) => {}
            },
            notice = notices.recv() => match notice {
                Ok(notice) => {
                    if notice.audience.as_ref().is_none_or(|audience| *audience == user) {
                        send(&mut socket, &notice.body).await?;
                    }
                }
                Err(RecvError::Closed) => return Ok(()),
                Err(RecvError::Lagged(_)) => {
                    socket
                        .send(Message::Close(Some(CloseFrame {
                            code: 1013,
                            reason: "reconnect".into(),
                        })))
                        .await?;
                    return Err(SocketError::Lagged);
                }
            }
        }
    }
}
