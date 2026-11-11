use std::str::FromStr as _;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use kernel::memory::share::desk;
use wire::gateway::share as body;

use super::NO_STORE;
use super::Refusal;
use super::ShareCaller;
use super::ShareJson;
use crate::Gateway;
use crate::auth::FreshLogin;

pub(super) async fn list(
    State(gateway): State<Arc<Gateway>>,
    _caller: ShareCaller,
) -> Result<Json<body::ServersReply>, Refusal> {
    let servers = gateway.share_desk()?.servers().await?;
    Ok(Json(body::ServersReply {
        servers: servers
            .servers
            .into_iter()
            .map(|server| body::ServerView {
                origin: server.origin.to_string(),
                kind: match server.kind {
                    desk::ServerKind::Cloud => body::ServerKind::Cloud,
                    desk::ServerKind::Team => body::ServerKind::Team,
                },
                user: server.user,
                signed_in: server.signed_in,
                default: server.default,
            })
            .collect(),
        logins_file: servers.logins_file,
    }))
}

pub(super) async fn login(
    State(gateway): State<Arc<Gateway>>,
    _caller: ShareCaller,
    _fresh: FreshLogin,
    ShareJson(request): ShareJson<body::LoginRequest>,
) -> Result<Response, Refusal> {
    let request = match request {
        body::LoginRequest::Cloud { origin } => desk::LoginRequest::Cloud {
            origin: origin.into(),
        },
        body::LoginRequest::Team { origin, token } => desk::LoginRequest::Team {
            origin: origin.into(),
            token,
        },
    };
    let step = gateway.share_desk()?.login(request).await?;
    Ok(step_response(step))
}

pub(super) async fn poll(
    State(gateway): State<Arc<Gateway>>,
    _caller: ShareCaller,
    ShareJson(request): ShareJson<body::PollRequest>,
) -> Result<Response, Refusal> {
    let login = desk::LoginId::from_str(&request.login)?;
    Ok(step_response(gateway.share_desk()?.poll(login).await?))
}

pub(super) async fn logout(
    State(gateway): State<Arc<Gateway>>,
    _caller: ShareCaller,
    _fresh: FreshLogin,
    ShareJson(request): ShareJson<body::LogoutRequest>,
) -> Result<StatusCode, Refusal> {
    gateway.share_desk()?.logout(request.origin.into()).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn key(
    State(gateway): State<Arc<Gateway>>,
    _caller: ShareCaller,
) -> Result<Json<body::KeyReply>, Refusal> {
    let key = gateway.share_desk()?.key().await?;
    Ok(Json(body::KeyReply {
        key: key.key,
        fingerprint: key.fingerprint,
    }))
}

fn step_response(step: desk::LoginStep) -> Response {
    let step = match step {
        desk::LoginStep::Started {
            login,
            user_code,
            verification_uri_complete,
            interval,
            expires_at,
        } => body::LoginStep::Started {
            login: login.to_string(),
            user_code,
            verification_uri_complete,
            interval,
            expires_at,
        },
        desk::LoginStep::Pending { interval } => body::LoginStep::Pending { interval },
        desk::LoginStep::SignedIn { origin, user } => body::LoginStep::SignedIn {
            origin: origin.to_string(),
            user,
        },
    };
    (NO_STORE, Json(step)).into_response()
}
