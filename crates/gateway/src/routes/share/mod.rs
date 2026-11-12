mod send;
mod servers;

use std::str::FromStr as _;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::FromRequest;
use axum::extract::FromRequestParts;
use axum::extract::Request;
use axum::http::HeaderName;
use axum::http::StatusCode;
use axum::http::header;
use axum::http::request::Parts;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::{get, post};
use kernel::memory::share::desk::DeskError;
use kernel::memory::share::desk::DraftId;
use serde::de::DeserializeOwned;
use wire::gateway::share::RefusalReason;
use wire::gateway::share::ShareRefusal;

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::limited_bytes;
use crate::tenancy::Credential;

const NOT_LOCAL: &str = "sharing runs only on the machine rh runs on";
const FAILED: &str = "sharing failed; the rh log names the cause";
const NO_STORE: [(HeaderName, &str); 1] = [(header::CACHE_CONTROL, "no-store")];

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/api/share/find",
            post(send::find).fallback(super::method_not_allowed),
        )
        .route(
            "/api/share/drafts",
            post(send::draft).fallback(super::method_not_allowed),
        )
        .route(
            "/api/share/drafts/{id}",
            get(send::report)
                .delete(send::discard)
                .fallback(super::method_not_allowed),
        )
        .route(
            "/api/share/drafts/{id}/marks",
            post(send::mark).fallback(super::method_not_allowed),
        )
        .route(
            "/api/share/drafts/{id}/send",
            post(send::send).fallback(super::method_not_allowed),
        )
        .route(
            "/api/share/servers",
            get(servers::list).fallback(super::method_not_allowed),
        )
        .route(
            "/api/share/servers/login",
            post(servers::login).fallback(super::method_not_allowed),
        )
        .route(
            "/api/share/servers/login/poll",
            post(servers::poll).fallback(super::method_not_allowed),
        )
        .route(
            "/api/share/servers/logout",
            post(servers::logout).fallback(super::method_not_allowed),
        )
        .route(
            "/api/share/key",
            get(servers::key).fallback(super::method_not_allowed),
        )
        .layer(DefaultBodyLimit::max(super::BODY_LIMIT))
}

#[derive(Debug)]
pub(super) enum Refusal {
    Share(StatusCode, ShareRefusal),
    Api(ApiError),
}

impl Refusal {
    fn new(status: StatusCode, reason: RefusalReason, error: impl Into<String>) -> Self {
        Self::Share(
            status,
            ShareRefusal {
                error: error.into(),
                reason,
                upstream_status: None,
            },
        )
    }

    fn not_local(error: &str) -> Self {
        Self::new(StatusCode::FORBIDDEN, RefusalReason::NotLocal, error)
    }

    fn invalid(error: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, RefusalReason::Invalid, error)
    }
}

impl From<ApiError> for Refusal {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl From<DeskError> for Refusal {
    fn from(error: DeskError) -> Self {
        let message = error.to_string();
        let (status, reason, upstream_status) = match &error {
            DeskError::UnknownDraft => (StatusCode::NOT_FOUND, RefusalReason::UnknownDraft, None),
            DeskError::UnknownLogin => (StatusCode::NOT_FOUND, RefusalReason::UnknownLogin, None),
            DeskError::UnknownSessions(_) => {
                (StatusCode::NOT_FOUND, RefusalReason::UnknownSessions, None)
            }
            DeskError::OpenFlags { .. } => (StatusCode::CONFLICT, RefusalReason::OpenFlags, None),
            DeskError::Counts { .. } => (StatusCode::CONFLICT, RefusalReason::Counts, None),
            DeskError::DecideUnconfigured => (
                StatusCode::CONFLICT,
                RefusalReason::DecideUnconfigured,
                None,
            ),
            DeskError::Full => (StatusCode::TOO_MANY_REQUESTS, RefusalReason::Full, None),
            DeskError::Choices(_) | DeskError::Marks(_) => {
                (StatusCode::BAD_REQUEST, RefusalReason::Invalid, None)
            }
            DeskError::BadToken(_) => (StatusCode::BAD_REQUEST, RefusalReason::BadToken, None),
            DeskError::BadServer(_) => (StatusCode::BAD_REQUEST, RefusalReason::BadServer, None),
            DeskError::SignedOut(_) => (StatusCode::UNAUTHORIZED, RefusalReason::SignedOut, None),
            DeskError::LoginExpired(_) => {
                (StatusCode::UNAUTHORIZED, RefusalReason::LoginExpired, None)
            }
            DeskError::LoginDenied(_) => (StatusCode::FORBIDDEN, RefusalReason::LoginDenied, None),
            DeskError::DropBox { status, .. } => (
                StatusCode::BAD_GATEWAY,
                RefusalReason::DropBox,
                Some(*status),
            ),
            DeskError::Unreachable(_) => {
                (StatusCode::BAD_GATEWAY, RefusalReason::Unreachable, None)
            }
            DeskError::Failed(_) => {
                tracing::error!(%error, "share request failed");
                return Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    RefusalReason::Failed,
                    FAILED,
                );
            }
        };
        Self::Share(
            status,
            ShareRefusal {
                error: message,
                reason,
                upstream_status,
            },
        )
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Self::Share(status, body) => (status, NO_STORE, Json(body)).into_response(),
            Self::Api(error) => error.into_response(),
        }
    }
}

pub(super) struct ShareCaller;

impl FromRequestParts<Arc<Gateway>> for ShareCaller {
    type Rejection = Refusal;

    fn from_request_parts(
        parts: &mut Parts,
        gateway: &Arc<Gateway>,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(admit(gateway, parts))
    }
}

fn admit(gateway: &Gateway, parts: &Parts) -> Result<ShareCaller, Refusal> {
    let plugin = parts
        .extensions
        .get::<crate::plugins::PluginCaller>()
        .is_some();
    if plugin || !crate::auth::direct_local(parts) {
        return Err(Refusal::not_local(NOT_LOCAL));
    }
    let (user, credential) = crate::tenancy::authenticate(gateway, parts)?;
    let owner_credential = match credential {
        Credential::Session { .. } => true,
        Credential::Bearer => crate::tenancy::owner_token_presented(gateway, parts),
        Credential::Plugin => false,
    };
    if !owner_credential || gateway.tenancy.local_user() != Some(&user) {
        return Err(Refusal::not_local(NOT_LOCAL));
    }
    Ok(ShareCaller)
}

pub(super) struct ShareJson<T>(pub(super) T);

impl<T> FromRequest<Arc<Gateway>> for ShareJson<T>
where
    T: DeserializeOwned,
{
    type Rejection = Refusal;

    async fn from_request(request: Request, state: &Arc<Gateway>) -> Result<Self, Self::Rejection> {
        let bytes = limited_bytes(request, state).await?;
        serde_json::from_slice(&bytes)
            .map(Self)
            .map_err(|error| Refusal::invalid(error.to_string()))
    }
}

fn draft_id(raw: &str) -> Result<DraftId, Refusal> {
    Ok(DraftId::from_str(raw)?)
}
