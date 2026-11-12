mod receive;
mod trust;

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
use kernel::memory::share::receive::ReceiveError;
use serde::de::DeserializeOwned;
use wire::gateway::share_receive::ReceiveReason;
use wire::gateway::share_receive::ReceiveRefusal;

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::limited_bytes;

const NOT_LOCAL: &str = "receiving runs only on the machine rh runs on";
const FAILED: &str = "receiving failed; the rh log names the cause";
const NO_STORE: [(HeaderName, &str); 1] = [(header::CACHE_CONTROL, "no-store")];

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/api/share/receive",
            post(receive::inspect).fallback(super::method_not_allowed),
        )
        .route(
            "/api/share/receive/import",
            post(receive::import).fallback(super::method_not_allowed),
        )
        .route(
            "/api/share/trust",
            get(trust::list)
                .post(trust::add)
                .delete(trust::remove)
                .fallback(super::method_not_allowed),
        )
        .layer(DefaultBodyLimit::max(super::BODY_LIMIT))
}

#[derive(Debug)]
pub(super) enum Refusal {
    Receive(StatusCode, ReceiveRefusal),
    Api(ApiError),
}

impl Refusal {
    fn new(status: StatusCode, reason: ReceiveReason, error: impl Into<String>) -> Self {
        Self::Receive(
            status,
            ReceiveRefusal {
                error: error.into(),
                reason,
                sessions: None,
            },
        )
    }

    fn not_local() -> Self {
        Self::new(StatusCode::FORBIDDEN, ReceiveReason::NotLocal, NOT_LOCAL)
    }

    fn invalid(error: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ReceiveReason::Invalid, error)
    }
}

impl From<ApiError> for Refusal {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl From<ReceiveError> for Refusal {
    fn from(error: ReceiveError) -> Self {
        use ReceiveReason as Reason;
        let message = error.to_string();
        let sessions = held_sessions(&error);
        let (status, reason) = match &error {
            ReceiveError::BadCode(_) => (StatusCode::BAD_REQUEST, Reason::BadCode),
            ReceiveError::AlreadyReceived(_) => (StatusCode::CONFLICT, Reason::AlreadyReceived),
            ReceiveError::Gone(_) => (StatusCode::NOT_FOUND, Reason::Gone),
            ReceiveError::Burned(_) => (StatusCode::GONE, Reason::Burned),
            ReceiveError::TooLarge => (StatusCode::BAD_GATEWAY, Reason::TooLarge),
            ReceiveError::Unreachable(_) => (StatusCode::BAD_GATEWAY, Reason::Unreachable),
            ReceiveError::Refused(_) => (StatusCode::FORBIDDEN, Reason::Refused),
            ReceiveError::Undecryptable => {
                (StatusCode::UNPROCESSABLE_ENTITY, Reason::Undecryptable)
            }
            ReceiveError::Unreadable(_) => (StatusCode::UNPROCESSABLE_ENTITY, Reason::Unreadable),
            ReceiveError::BadSignature(_) => {
                (StatusCode::UNPROCESSABLE_ENTITY, Reason::BadSignature)
            }
            ReceiveError::Raw => (StatusCode::UNPROCESSABLE_ENTITY, Reason::Raw),
            ReceiveError::Withheld(_) => (StatusCode::UNPROCESSABLE_ENTITY, Reason::Withheld),
            ReceiveError::NoSuchSession(_) => (StatusCode::BAD_REQUEST, Reason::NoSuchSession),
            ReceiveError::Undecided(_) => (StatusCode::CONFLICT, Reason::Undecided),
            ReceiveError::SecretImport(_) => (StatusCode::CONFLICT, Reason::SecretImport),
            ReceiveError::AuthorFindings => (StatusCode::CONFLICT, Reason::AuthorFindings),
            ReceiveError::FreshLoginRequired => return Self::Api(ApiError::ReauthRequired),
            ReceiveError::BadKey(_) => (StatusCode::BAD_REQUEST, Reason::BadKey),
            ReceiveError::BadName(_) => (StatusCode::BAD_REQUEST, Reason::BadName),
            ReceiveError::Duplicate(_) => (StatusCode::CONFLICT, Reason::Duplicate),
            ReceiveError::Failed(_) => {
                tracing::error!(%error, "receive request failed");
                return Self::new(StatusCode::INTERNAL_SERVER_ERROR, Reason::Failed, FAILED);
            }
        };
        Self::Receive(
            status,
            ReceiveRefusal {
                error: message,
                reason,
                sessions,
            },
        )
    }
}

fn held_sessions(error: &ReceiveError) -> Option<Vec<usize>> {
    match error {
        ReceiveError::Undecided(numbers) | ReceiveError::SecretImport(numbers) => {
            Some(numbers.clone())
        }
        ReceiveError::BadCode(_)
        | ReceiveError::AlreadyReceived(_)
        | ReceiveError::Gone(_)
        | ReceiveError::Burned(_)
        | ReceiveError::TooLarge
        | ReceiveError::Unreachable(_)
        | ReceiveError::Refused(_)
        | ReceiveError::Undecryptable
        | ReceiveError::Unreadable(_)
        | ReceiveError::BadSignature(_)
        | ReceiveError::Raw
        | ReceiveError::Withheld(_)
        | ReceiveError::NoSuchSession(_)
        | ReceiveError::AuthorFindings
        | ReceiveError::FreshLoginRequired
        | ReceiveError::BadKey(_)
        | ReceiveError::BadName(_)
        | ReceiveError::Duplicate(_)
        | ReceiveError::Failed(_) => None,
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Self::Receive(status, body) => (status, NO_STORE, Json(body)).into_response(),
            Self::Api(error) => error.into_response(),
        }
    }
}

pub(super) struct ReceiveCaller;

impl FromRequestParts<Arc<Gateway>> for ReceiveCaller {
    type Rejection = Refusal;

    fn from_request_parts(
        parts: &mut Parts,
        gateway: &Arc<Gateway>,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(admit(gateway, parts))
    }
}

fn admit(gateway: &Gateway, parts: &Parts) -> Result<ReceiveCaller, Refusal> {
    let plugin = parts
        .extensions
        .get::<crate::plugins::PluginCaller>()
        .is_some();
    if plugin || !crate::auth::direct_local(parts) {
        return Err(Refusal::not_local());
    }
    let user = crate::tenancy::caller(gateway, parts)?;
    if gateway.tenancy.local_user() != Some(&user) {
        return Err(Refusal::not_local());
    }
    Ok(ReceiveCaller)
}

pub(super) struct ReceiveJson<T>(pub(super) T);

impl<T> FromRequest<Arc<Gateway>> for ReceiveJson<T>
where
    T: DeserializeOwned,
{
    type Rejection = Refusal;

    async fn from_request(request: Request, state: &Arc<Gateway>) -> Result<Self, Self::Rejection> {
        let bytes = limited_bytes(request, state).await?;
        serde_json::from_slice(&bytes).map(Self).map_err(|error| {
            Refusal::invalid(format!(
                "the request body is not valid for this route (line {}, column {})",
                error.line(),
                error.column()
            ))
        })
    }
}
