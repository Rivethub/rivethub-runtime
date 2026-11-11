mod proxy;

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use wire::gateway::GatewayError;
use wire::plugin::host::{
    PluginEnableRequest, PluginInstallRequest, PluginList, PluginLogs, PluginRecord,
    PluginSettings, PluginSettingsUpdate,
};
use wire::plugin::{PluginId, RouteName};

use crate::Gateway;
use crate::extract::Body;
use crate::plugins::{HostError, PluginHost};
use crate::tenancy::UserId;

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/api/plugins",
            get(list).post(install).fallback(super::method_not_allowed),
        )
        .route(
            "/api/plugins/{id}",
            get(show).delete(remove).fallback(super::method_not_allowed),
        )
        .route(
            "/api/plugins/{id}/enable",
            post(enable).fallback(super::method_not_allowed),
        )
        .route(
            "/api/plugins/{id}/disable",
            post(disable).fallback(super::method_not_allowed),
        )
        .route(
            "/api/plugins/{id}/reload",
            post(reload).fallback(super::method_not_allowed),
        )
        .route(
            "/api/plugins/{id}/logs",
            get(logs).fallback(super::method_not_allowed),
        )
        .route(
            "/api/plugins/{id}/settings",
            get(settings)
                .put(change_settings)
                .fallback(super::method_not_allowed),
        )
        .route(
            "/api/plugins/{id}/jobs/{job}/run",
            post(run_job).fallback(super::method_not_allowed),
        )
        .route("/api/plugins/{id}/{kind}/{*rest}", any(proxy::forward))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PluginRouteError {
    #[error(transparent)]
    Host(#[from] HostError),
    #[error("the plug-in host is not running on this node")]
    NoHost,
    #[error("only the node's owner manages plug-ins")]
    NotOwner,
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    TooLarge(String),
    #[error("the plug-in answered with an invalid body")]
    Invalid(String),
    #[error("{0}")]
    Failed(String),
    #[error("{0}")]
    Timeout(String),
    #[error("rh cannot name this caller to the plug-in")]
    Unnamed(String),
}

impl IntoResponse for PluginRouteError {
    fn into_response(self) -> Response {
        let (status, code, detail) = match &self {
            Self::Host(HostError::NotFound(_)) | Self::NoHost | Self::NotFound(_) => {
                (StatusCode::NOT_FOUND, "not_found", None)
            }
            Self::Host(HostError::Invalid(_)) | Self::BadRequest(_) => {
                (StatusCode::BAD_REQUEST, "invalid", None)
            }
            Self::Host(HostError::Conflict(_)) => (StatusCode::CONFLICT, "conflict", None),
            Self::Host(HostError::Unconfinable(_)) => {
                (StatusCode::CONFLICT, "sandbox_unavailable", None)
            }
            Self::Host(HostError::Unavailable(_)) => {
                (StatusCode::SERVICE_UNAVAILABLE, "plugin_unavailable", None)
            }
            Self::Host(HostError::Approval(kernel::plugins::ApprovalError::Unapproved(
                missing,
            ))) => (
                StatusCode::CONFLICT,
                "permissions_unapproved",
                Some(names(missing)),
            ),
            Self::Host(HostError::Approval(kernel::plugins::ApprovalError::NotPending(extra))) => {
                (StatusCode::BAD_REQUEST, "not_pending", Some(names(extra)))
            }
            Self::Host(HostError::Io(_) | HostError::Json(_)) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal", None)
            }
            Self::NotOwner => (StatusCode::FORBIDDEN, "owner_required", None),
            Self::TooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, "body_too_large", None),
            Self::Invalid(detail) => (
                StatusCode::BAD_GATEWAY,
                "plugin_invalid_response",
                Some(detail.clone()),
            ),
            Self::Failed(_) => (StatusCode::BAD_GATEWAY, "plugin_error", None),
            Self::Timeout(_) => (StatusCode::GATEWAY_TIMEOUT, "plugin_timeout", None),
            Self::Unnamed(detail) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                Some(detail.clone()),
            ),
        };
        let mut body = GatewayError::new(self.to_string()).code(code);
        if let Some(detail) = detail {
            body = body.detail(detail);
        }
        if status == StatusCode::SERVICE_UNAVAILABLE {
            body = body.retryable(true);
        }
        (status, Json(body)).into_response()
    }
}

fn names(permissions: &[kernel::plugins::Permission]) -> String {
    permissions
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn host(gateway: &Gateway) -> Result<&Arc<PluginHost>, PluginRouteError> {
    gateway.plugins().ok_or(PluginRouteError::NoHost)
}

fn owner(gateway: &Gateway, user: &UserId) -> Result<(), PluginRouteError> {
    if gateway.tenancy.local_user() == Some(user) {
        Ok(())
    } else {
        Err(PluginRouteError::NotOwner)
    }
}

fn plugin_id(raw: String) -> Result<PluginId, PluginRouteError> {
    PluginId::try_from(raw).map_err(|error| PluginRouteError::NotFound(error.to_string()))
}

async fn list(
    State(gateway): State<Arc<Gateway>>,
    _user: UserId,
) -> Result<Json<PluginList>, PluginRouteError> {
    Ok(Json(host(&gateway)?.list()))
}

async fn show(
    State(gateway): State<Arc<Gateway>>,
    _user: UserId,
    Path(id): Path<String>,
) -> Result<Json<PluginRecord>, PluginRouteError> {
    Ok(Json(host(&gateway)?.record(&plugin_id(id)?)?))
}

async fn install(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Body(request): Body<PluginInstallRequest>,
) -> Result<(StatusCode, Json<PluginRecord>), PluginRouteError> {
    owner(&gateway, &user)?;
    let record = host(&gateway)?
        .install(std::path::Path::new(&request.path))
        .await?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn enable(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(id): Path<String>,
    Body(request): Body<PluginEnableRequest>,
) -> Result<Json<PluginRecord>, PluginRouteError> {
    owner(&gateway, &user)?;
    Ok(Json(
        host(&gateway)?
            .enable(&plugin_id(id)?, &request.approve)
            .await?,
    ))
}

async fn disable(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(id): Path<String>,
) -> Result<Json<PluginRecord>, PluginRouteError> {
    owner(&gateway, &user)?;
    Ok(Json(host(&gateway)?.disable(&plugin_id(id)?).await?))
}

async fn reload(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(id): Path<String>,
) -> Result<Json<PluginRecord>, PluginRouteError> {
    owner(&gateway, &user)?;
    Ok(Json(host(&gateway)?.reload(&plugin_id(id)?).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveQuery {
    keep_data: Option<bool>,
}

async fn remove(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(id): Path<String>,
    Query(query): Query<RemoveQuery>,
) -> Result<StatusCode, PluginRouteError> {
    owner(&gateway, &user)?;
    host(&gateway)?
        .remove(&plugin_id(id)?, query.keep_data == Some(true))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct LogsQuery {
    tail: Option<usize>,
}

async fn logs(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<PluginLogs>, PluginRouteError> {
    owner(&gateway, &user)?;
    let lines = host(&gateway)?.tail(&plugin_id(id)?, query.tail).await?;
    Ok(Json(PluginLogs { lines }))
}

async fn settings(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(id): Path<String>,
) -> Result<Json<PluginSettings>, PluginRouteError> {
    owner(&gateway, &user)?;
    Ok(Json(host(&gateway)?.plugin_settings(&plugin_id(id)?)?))
}

async fn change_settings(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(id): Path<String>,
    Body(request): Body<PluginSettingsUpdate>,
) -> Result<Json<PluginSettings>, PluginRouteError> {
    owner(&gateway, &user)?;
    let host = host(&gateway)?;
    Ok(Json(host.change_settings(&plugin_id(id)?, &request).await?))
}

async fn run_job(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path((id, job)): Path<(String, String)>,
) -> Result<(StatusCode, Json<wire::plugin::host::PluginJobRun>), PluginRouteError> {
    owner(&gateway, &user)?;
    let job =
        RouteName::try_from(job).map_err(|error| PluginRouteError::NotFound(error.to_string()))?;
    let run = host(&gateway)?.start_job(&plugin_id(id)?, &job)?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}
