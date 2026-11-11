mod account;
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
        .merge(model_calls::router())
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

pub(crate) async fn healthz(State(gateway): State<Arc<Gateway>>) -> axum::response::Response {
    let name = gateway.settings().hostname.clone();
    let node = gateway.settings().node.clone();
    let degraded_providers = degraded_providers(gateway.provider_health());
    match gateway.store().probe().await {
        Ok(()) => (
            StatusCode::OK,
            Json(HealthzResponse {
                ok: true,
                sessions: 0,
                name,
                node,
                degraded: None,
                degraded_providers,
                version: Some(crate::VERSION.to_owned()),
                revision: crate::REVISION.map(str::to_owned),
            }),
        )
            .into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthzResponse {
                ok: false,
                sessions: 0,
                name,
                node,
                degraded: Some(degraded_reason(&error)),
                degraded_providers,
                version: Some(crate::VERSION.to_owned()),
                revision: crate::REVISION.map(str::to_owned),
            }),
        )
            .into_response(),
    }
}

fn degraded_providers(providers: &[ProviderHealth]) -> Vec<DegradedProvider> {
    providers
        .iter()
        .filter_map(|provider| match &*provider.status.borrow() {
            ProviderStatus::Ready => None,
            ProviderStatus::Degraded(degradation) => Some(DegradedProvider {
                id: provider.id.clone(),
                recovery: recovery_body(degradation.recovery),
                reason: degradation.cause.to_string(),
            }),
        })
        .collect()
}

const fn recovery_body(recovery: ProviderRecovery) -> wire::gateway::ProviderRecovery {
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
