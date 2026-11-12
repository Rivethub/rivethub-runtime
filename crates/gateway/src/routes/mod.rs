mod account;
mod auth;
pub mod capture;
pub mod capture_schema;
pub(crate) mod capture_status;
mod entities;
mod flagged;
pub mod harnesses;
mod history;
pub mod memory;
mod memory_archive;
mod memory_models;
mod memory_snippet;
pub(crate) mod model_calls;
pub(crate) mod notices;
mod notifications;
mod plugins;
mod review;
mod session_delete;
mod share;
pub mod tags;
mod tidy;
mod today;
pub(crate) mod topics;
pub mod wiki;

mod surface;

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use kernel::memory::MemoryStoreError;
use kernel::model::{ProviderHealth, ProviderRecovery, ProviderStatus};
use wire::gateway::{DegradedProvider, HealthzResponse};

use crate::Gateway;
use crate::error::ApiError;

const PUBLIC_DEGRADED: &str = "database unreachable";
const PUBLIC_PROVIDER_REASON: &str = "unavailable; see the gateway log";

pub(crate) const BODY_LIMIT: usize = 256 * 1024;

pub(crate) async fn contract_header(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    let text = wire::contract::MEMORY_CONTRACT.to_string();
    if let Ok(value) = axum::http::HeaderValue::try_from(text) {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static("x-rivethub-contract"),
            value,
        );
    }
    response
}

pub(crate) fn router() -> axum::Router<Arc<Gateway>> {
    harnesses::mount(wiki::router())
        .merge(auth::router())
        .merge(notices::router())
        .merge(account::router())
        .merge(history::router())
        .merge(notifications::router())
        .merge(topics::router())
        .merge(entities::router())
        .merge(review::router())
        .merge(tidy::router())
        .merge(today::router())
        .merge(plugins::router())
        .merge(session_delete::router())
        .merge(share::router())
        .merge(model_calls::router())
}

pub(crate) fn serves_public_asset(method: &axum::http::Method, path: &str) -> bool {
    surface::serves_public_asset(method, path)
}

pub(crate) fn mount(router: Router<Arc<Gateway>>) -> Router<Arc<Gateway>> {
    surface::mount(router)
}

pub(crate) fn join_task<T>(joined: Result<T, tokio::task::JoinError>) -> Result<T, ApiError> {
    match joined {
        Ok(value) => Ok(value),
        Err(error) => {
            tracing::error!("blocking task failed: {error}");
            Err(ApiError::Task(error))
        }
    }
}

pub(crate) async fn healthz(
    State(gateway): State<Arc<Gateway>>,
    parts: axum::http::request::Parts,
) -> axum::response::Response {
    let detailed = crate::tenancy::caller(&gateway, &parts).is_ok();
    let name = gateway.settings().hostname.clone();
    let node = gateway.settings().node.clone();
    let degraded_providers = degraded_providers(gateway.provider_health(), detailed);
    let probed = gateway.store().probe().await;
    let (status, ok, degraded) = match probed {
        Ok(()) => (StatusCode::OK, true, None),
        Err(error) => {
            let reason = degraded_reason(&error);
            tracing::warn!("{reason}");
            let shown = if detailed {
                reason
            } else {
                PUBLIC_DEGRADED.to_owned()
            };
            (StatusCode::SERVICE_UNAVAILABLE, false, Some(shown))
        }
    };
    (
        status,
        Json(HealthzResponse {
            ok,
            sessions: 0,
            name,
            node,
            degraded,
            degraded_providers,
            version: Some(crate::VERSION.to_owned()),
            revision: crate::REVISION.filter(|_| detailed).map(str::to_owned),
            policy: gateway.settings().policy.clone(),
        }),
    )
        .into_response()
}

fn degraded_providers(providers: &[ProviderHealth], detailed: bool) -> Vec<DegradedProvider> {
    providers
        .iter()
        .filter_map(|provider| match &*provider.status.borrow() {
            ProviderStatus::Ready => None,
            ProviderStatus::Degraded(degradation) => Some(DegradedProvider {
                id: provider.id.clone(),
                recovery: recovery_body(degradation.recovery),
                reason: if detailed {
                    degradation.cause.to_string()
                } else {
                    PUBLIC_PROVIDER_REASON.to_owned()
                },
            }),
        })
        .collect()
}

fn recovery_body(recovery: ProviderRecovery) -> wire::gateway::ProviderRecovery {
    match recovery {
        ProviderRecovery::Retrying => wire::gateway::ProviderRecovery::Retrying,
        ProviderRecovery::Permanent => wire::gateway::ProviderRecovery::Permanent,
    }
}

fn degraded_reason(error: &MemoryStoreError) -> String {
    let detail = match error {
        MemoryStoreError::Unavailable(source) => source.to_string(),
        MemoryStoreError::Query { message, .. } | MemoryStoreError::InvalidRequest { message } => {
            message.clone()
        }
        MemoryStoreError::TagTablesMissing => error.to_string(),
    };
    format!("database unreachable: {detail}")
}

pub(crate) async fn not_found() -> ApiError {
    ApiError::NotFound("not found")
}

pub(crate) async fn method_not_allowed() -> ApiError {
    ApiError::MethodNotAllowed
}
