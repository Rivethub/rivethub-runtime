use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse as _;
use axum::response::Response;
use kernel::memory::share::receive as port;
use wire::gateway::share_receive as body;

use super::NO_STORE;
use super::ReceiveCaller;
use super::ReceiveJson;
use super::Refusal;
use crate::Gateway;
use crate::auth::FreshLogin;

pub(super) fn sender(sender: port::Sender) -> body::TrustedSender {
    body::TrustedSender {
        name: sender.name,
        key: sender.key,
        fingerprint: sender.fingerprint,
    }
}

pub(super) async fn list(
    State(gateway): State<Arc<Gateway>>,
    _caller: ReceiveCaller,
) -> Result<Response, Refusal> {
    let senders = gateway.receive_desk()?.trusted().await?;
    Ok((
        NO_STORE,
        Json(body::TrustReply {
            senders: senders.into_iter().map(sender).collect(),
        }),
    )
        .into_response())
}

pub(super) async fn add(
    State(gateway): State<Arc<Gateway>>,
    _caller: ReceiveCaller,
    _fresh: FreshLogin,
    ReceiveJson(request): ReceiveJson<body::TrustRequest>,
) -> Result<Response, Refusal> {
    let added = gateway
        .receive_desk()?
        .trust(request.name, request.key)
        .await?;
    Ok((StatusCode::CREATED, NO_STORE, Json(sender(added))).into_response())
}

pub(super) async fn remove(
    State(gateway): State<Arc<Gateway>>,
    _caller: ReceiveCaller,
    ReceiveJson(request): ReceiveJson<body::UntrustRequest>,
) -> Result<Response, Refusal> {
    let removed = gateway
        .receive_desk()?
        .untrust(request.name, request.key)
        .await?;
    Ok((NO_STORE, Json(body::UntrustReply { removed })).into_response())
}
