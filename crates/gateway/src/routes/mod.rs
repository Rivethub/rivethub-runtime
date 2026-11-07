pub mod agents;
pub mod ca;
pub mod capture;
pub mod capture_schema;
pub(crate) mod capture_status;
pub mod chat;
pub mod devices;
pub mod files;
mod flagged;
pub mod harnesses;
mod history;
pub(crate) mod hub_state;
pub mod memory;
mod memory_archive;
mod memory_models;
mod memory_snippet;
mod notifications;
mod plugins;
mod session_delete;
pub mod sessions;
pub mod tags;
pub mod tasks;
pub mod terminal;
mod tidy;
pub(crate) mod topics;
mod updates;
pub(crate) mod voice;
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

pub(crate) use surface::MeshCache;
pub(crate) use surface::plain_get;
pub(crate) use surface::rewrite_request;

pub(crate) fn organizer() -> axum::Router<Arc<Gateway>> {
    wiki::router().merge(topics::router()).merge(tidy::router())
}

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
    harnesses::mount(files::mount(wiki::router()))
        .merge(voice::router())
        .merge(updates::router())
        .nest("/api/agents", agents::router())
        .merge(devices::router())
        .merge(ca::router())
        .merge(history::router())
        .merge(notifications::router())
        .merge(topics::router())
        .merge(tidy::router())
        .merge(hub_state::router())
        .merge(plugins::router())
        .merge(session_delete::router())
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
    let sessions = gateway.sessions().count() as u64;
    let name = gateway.settings().hostname.clone();
    let node = gateway.settings().node.clone();
    let degraded_providers = degraded_providers(gateway.provider_health());
    match gateway.store().probe().await {
        Ok(()) => (
            StatusCode::OK,
            Json(HealthzResponse {
                ok: true,
                sessions,
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
                sessions,
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
