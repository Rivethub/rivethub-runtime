use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use kernel::memory::share::desk::BundleName;
use wire::gateway::share as body;

use super::NO_STORE;
use super::Refusal;
use super::ShareCaller;
use super::ShareJson;
use super::send::report_body;
use crate::Gateway;

pub(super) async fn list(
    State(gateway): State<Arc<Gateway>>,
    _caller: ShareCaller,
) -> Result<Response, Refusal> {
    let bundles = gateway.share_desk()?.bundles().await?;
    let reply = body::BundlesReply {
        bundles: bundles
            .into_iter()
            .map(|bundle| body::ReadyBundle {
                name: bundle.name.to_string(),
                sessions: bundle.sessions,
                bytes: bundle.bytes,
                open_flags: bundle.open_flags,
                created_at: bundle.created_at,
                sanitized: true,
            })
            .collect(),
    };
    Ok((NO_STORE, Json(reply)).into_response())
}

pub(super) async fn draft(
    State(gateway): State<Arc<Gateway>>,
    _caller: ShareCaller,
    ShareJson(request): ShareJson<body::BundleDraftRequest>,
) -> Result<Response, Refusal> {
    let name: BundleName = request.name.parse()?;
    let drafted = gateway.share_desk()?.draft_bundle(name).await?;
    Ok((StatusCode::CREATED, NO_STORE, Json(report_body(drafted))).into_response())
}
