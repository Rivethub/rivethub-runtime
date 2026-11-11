use std::sync::Arc;
use std::sync::LazyLock;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::header;
use axum::http::request::Parts;
use axum::response::Html;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use base64::Engine as _;
use sha2::Digest as _;
use sha2::Sha256;
use tower_sessions::Session;
use wire::gateway::auth::LoginCodeResponse;
use wire::gateway::auth::RotateResponse;
use wire::gateway::auth::Secret;
use wire::gateway::auth::SessionRequest;
use wire::gateway::auth::SessionResponse;

use super::account::LocalOwner;
use crate::Gateway;
use crate::auth::FreshLogin;
use crate::error::ApiError;
use crate::extract::Body;
use crate::login::LOGIN_CODE_TTL;
use crate::login::MintError;
use crate::tenancy::UserId;

const PAGE_SCRIPT: &str = include_str!("login.js");
const PAGE_TEMPLATE: &str = include_str!("login.html");

struct Page {
    html: String,
    policy: String,
}

static PAGE: LazyLock<Page> = LazyLock::new(|| {
    let hash = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(PAGE_SCRIPT));
    Page {
        html: PAGE_TEMPLATE.replace("{{script}}", PAGE_SCRIPT),
        policy: format!(
            "default-src 'self'; script-src 'sha256-{hash}'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
        ),
    }
});

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route("/login", get(page).fallback(super::method_not_allowed))
        .route(
            "/api/auth/login-codes",
            post(mint).fallback(super::method_not_allowed),
        )
        .route(
            "/api/auth/session",
            post(exchange).fallback(super::method_not_allowed),
        )
        .route(
            "/api/auth/logout",
            post(logout).fallback(super::method_not_allowed),
        )
        .route(
            "/api/auth/rotate",
            post(rotate).fallback(super::method_not_allowed),
        )
}

async fn page() -> Response {
    let mut response = Html(PAGE.html.as_str()).into_response();
    if let Ok(policy) = HeaderValue::from_str(&PAGE.policy) {
        response
            .headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, policy);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn admit(gateway: &Gateway, parts: &Parts) -> Result<(), ApiError> {
    if !crate::auth::direct_local(parts) {
        return Err(ApiError::Unauthorized);
    }
    let peer = crate::login::peer_ip(parts).ok_or(ApiError::Unauthorized)?;
    gateway.browser().admit(peer).map_err(ApiError::RateLimited)
}

async fn mint(
    State(gateway): State<Arc<Gateway>>,
    parts: Parts,
) -> Result<Json<LoginCodeResponse>, ApiError> {
    admit(&gateway, &parts)?;
    if !crate::tenancy::owner_token_presented(&gateway, &parts) {
        return Err(ApiError::Unauthorized);
    }
    let code = gateway
        .browser()
        .codes
        .mint()
        .map_err(|error| match error {
            MintError::Full(wait) => ApiError::RateLimited(wait),
            MintError::Random => ApiError::Internal,
        })?;
    Ok(Json(LoginCodeResponse {
        code: Secret::new(code),
        expires_in_secs: LOGIN_CODE_TTL.as_secs(),
    }))
}

async fn exchange(
    State(gateway): State<Arc<Gateway>>,
    session: Session,
    parts: Parts,
    Body(request): Body<SessionRequest>,
) -> Result<Json<SessionResponse>, ApiError> {
    admit(&gateway, &parts)?;
    if !gateway.browser().codes.redeem(request.code.expose()) {
        return Err(ApiError::Unauthorized);
    }
    let csrf = gateway.browser().begin(&session).await?;
    Ok(Json(SessionResponse {
        csrf: Secret::new(csrf),
    }))
}

async fn logout(session: Session, _user: UserId) -> Result<StatusCode, ApiError> {
    session.flush().await.map_err(|_| ApiError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn rotate(
    State(gateway): State<Arc<Gateway>>,
    _fresh: FreshLogin,
    _owner: LocalOwner,
) -> Result<Json<RotateResponse>, ApiError> {
    let holder = gateway
        .local_owner()
        .ok_or_else(|| ApiError::Unavailable("no local owner token is configured".to_owned()))?;
    holder.rotate().map_err(|error| {
        tracing::error!(%error, "the local owner token could not be rotated");
        ApiError::Internal
    })?;
    gateway.browser().invalidate();
    Ok(Json(RotateResponse { rotated: true }))
}
