use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::FromRequestParts;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::{get, post};
use wire::gateway::GatewayError;
use wire::gateway::account::{
    Account, AccountUpdate, Hub, SignInProgress, SignInRequest, SignInStarted,
};

use crate::Gateway;
use crate::account::AccountError;
use crate::auth::FreshLogin;
use crate::error::ApiError;
use crate::extract::Body;
use crate::tenancy::UserId;

const LOCAL_ONLY: &str = "the account is managed only from the machine rh runs on";

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/api/account",
            get(show)
                .patch(change)
                .delete(delete)
                .fallback(super::method_not_allowed),
        )
        .route(
            "/api/account/signin",
            post(sign_in).fallback(super::method_not_allowed),
        )
        .route(
            "/api/account/signin/poll",
            post(poll).fallback(super::method_not_allowed),
        )
        .route(
            "/api/account/signout",
            post(sign_out).fallback(super::method_not_allowed),
        )
        .route("/api/hubs", get(hubs).fallback(super::method_not_allowed))
}

#[derive(Debug)]
enum Refused {
    Api(ApiError),
    Account(AccountError),
}

impl From<ApiError> for Refused {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl From<AccountError> for Refused {
    fn from(error: AccountError) -> Self {
        Self::Account(error)
    }
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        let error = match self {
            Self::Api(error) => return error.into_response(),
            Self::Account(error) => error,
        };
        let status = match &error {
            AccountError::Email => StatusCode::BAD_REQUEST,
            AccountError::NoSignIn
            | AccountError::NotSignedIn
            | AccountError::SignedIn
            | AccountError::SignedOutElsewhere
            | AccountError::Policy
            | AccountError::CloudPolicy => StatusCode::CONFLICT,
            AccountError::Expired => StatusCode::GONE,
            AccountError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            AccountError::CloudStatus(_)
            | AccountError::CloudReply
            | AccountError::Http(_)
            | AccountError::TooLarge
            | AccountError::Manifest
            | AccountError::Signature
            | AccountError::Checksum
            | AccountError::Platform => StatusCode::BAD_GATEWAY,
            AccountError::Url(_)
            | AccountError::UpdateKey
            | AccountError::Cancelled
            | AccountError::Io(_)
            | AccountError::State(_)
            | AccountError::Task(_) => {
                tracing::error!(%error, "account request failed");
                return ApiError::Internal.into_response();
            }
        };
        (status, Json(GatewayError::new(error.to_string()))).into_response()
    }
}

pub(super) struct LocalOwner;

impl FromRequestParts<Arc<Gateway>> for LocalOwner {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        gateway: &Arc<Gateway>,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(local_owner(gateway, parts))
    }
}

fn local_owner(gateway: &Gateway, parts: &Parts) -> Result<LocalOwner, ApiError> {
    let user = crate::tenancy::caller(gateway, parts)?;
    if !crate::auth::direct_local(parts) {
        return Err(ApiError::Forbidden(LOCAL_ONLY.to_owned()));
    }
    gateway.tenancy.owner(&user)?;
    Ok(LocalOwner)
}

async fn show(
    State(gateway): State<Arc<Gateway>>,
    _owner: LocalOwner,
) -> Result<Json<Account>, Refused> {
    Ok(Json(gateway.account()?.account().await))
}

async fn change(
    State(gateway): State<Arc<Gateway>>,
    _fresh: FreshLogin,
    _owner: LocalOwner,
    Body(body): Body<AccountUpdate>,
) -> Result<Json<Account>, Refused> {
    Ok(Json(gateway.account()?.update(body).await?))
}

async fn delete(
    State(gateway): State<Arc<Gateway>>,
    _fresh: FreshLogin,
    _owner: LocalOwner,
) -> Result<Json<wire::gateway::account::AccountDeleted>, Refused> {
    Ok(Json(gateway.account()?.delete().await?))
}

async fn sign_in(
    State(gateway): State<Arc<Gateway>>,
    _fresh: FreshLogin,
    _owner: LocalOwner,
    Body(_body): Body<SignInRequest>,
) -> Result<Json<SignInStarted>, Refused> {
    Ok(Json(gateway.account()?.sign_in().await?))
}

async fn poll(
    State(gateway): State<Arc<Gateway>>,
    _owner: LocalOwner,
) -> Result<Json<SignInProgress>, Refused> {
    Ok(Json(gateway.account()?.poll().await?))
}

async fn sign_out(
    State(gateway): State<Arc<Gateway>>,
    _fresh: FreshLogin,
    _owner: LocalOwner,
) -> Result<Json<Account>, Refused> {
    Ok(Json(gateway.account()?.sign_out().await?))
}

async fn hubs(
    State(gateway): State<Arc<Gateway>>,
    _user: UserId,
) -> Result<Json<Vec<Hub>>, Refused> {
    Ok(Json(gateway.account()?.hubs().await))
}
