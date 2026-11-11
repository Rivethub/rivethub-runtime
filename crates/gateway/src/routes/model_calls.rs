use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use kernel::model_calls::{CallTotals, ModelCall, ProxyHeaders, correlate};
use wire::model_call::{
    CallUsage, ModelCallBatchRequest, ModelCallBatchResult, SessionModelCalls, StoredModelCall,
};
use wire::session_id::SessionKey;

use super::session_delete::{segment_key, visible};
use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;
use crate::tenancy::UserId;

const NOT_FOUND: &str = "not found";

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new().route(
        "/api/sessions/{session}/model-calls",
        get(read).fallback(super::method_not_allowed),
    )
}

pub(crate) async fn ingest(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Body(request): Body<ModelCallBatchRequest>,
) -> Result<Json<ModelCallBatchResult>, ApiError> {
    let calls = request
        .calls
        .into_iter()
        .enumerate()
        .map(|(index, call)| translate(index, call))
        .collect::<Result<Vec<_>, _>>()?;
    let recorded = gateway.model_calls()?.record(user.owner(), &calls).await?;
    let matched = calls.iter().filter(|call| call.session.is_some()).count() as u64;
    Ok(Json(ModelCallBatchResult {
        ok: true,
        inserted: recorded.inserted,
        duplicate: recorded.duplicate,
        matched,
        unmatched: calls.len() as u64 - matched,
    }))
}

pub(crate) async fn read(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<Json<SessionModelCalls>, ApiError> {
    let SessionKey::Harness(session) = segment_key(&segment)? else {
        return Err(ApiError::NotFound(NOT_FOUND));
    };
    if !visible(&gateway, &user, &session).await? {
        return Err(ApiError::NotFound(NOT_FOUND));
    }
    let calls = gateway.model_calls()?.session_calls(&session).await?;
    let totals = CallTotals::of(&calls);
    Ok(Json(SessionModelCalls {
        totals: wire::model_call::CallTotals {
            calls: totals.calls,
            errors: totals.errors,
            usage: totals.usage,
            models: totals.models,
        },
        calls: calls.into_iter().map(stored).collect(),
    }))
}

fn translate(
    index: usize,
    call: wire::model_call::CapturedModelCall,
) -> Result<ModelCall, ApiError> {
    if call.call_id.trim().is_empty() {
        return Err(refused(index, "call_id must not be empty"));
    }
    if !counts_fit(call.usage, call.retry_after_secs) {
        return Err(refused(index, "a count exceeds 9223372036854775807"));
    }
    let headers = ProxyHeaders::parse(call.headers).map_err(|header| {
        refused(
            index,
            &format!("credential header {} is refused", header.name),
        )
    })?;
    let correlated = correlate(&headers);
    Ok(ModelCall {
        call_id: call.call_id,
        started_at: call.started_at,
        finished_at: call.finished_at,
        provider: call.provider,
        model_requested: call.model_requested,
        model_served: call.model_served,
        account: call.account,
        status_code: call.status_code,
        outcome: call.outcome,
        retry_after_secs: call.retry_after_secs,
        usage: call.usage,
        session: correlated.as_ref().map(|found| found.session.clone()),
        turn: correlated.and_then(|found| found.turn),
    })
}

fn refused(index: usize, reason: &str) -> ApiError {
    ApiError::BadRequest(format!("calls[{index}]: {reason}"))
}

fn counts_fit(usage: Option<CallUsage>, retry_after_secs: Option<u64>) -> bool {
    let limit = i64::MAX.unsigned_abs();
    let usage = usage.unwrap_or_default();
    [
        retry_after_secs.unwrap_or_default(),
        usage.input,
        usage.output,
        usage.cache_read,
        usage.cache_write,
        usage.reasoning,
    ]
    .into_iter()
    .all(|count| count <= limit)
}

fn stored(call: ModelCall) -> StoredModelCall {
    StoredModelCall {
        call_id: call.call_id,
        started_at: call.started_at,
        finished_at: call.finished_at,
        provider: call.provider,
        model_requested: call.model_requested,
        model_served: call.model_served,
        account: call.account,
        status_code: call.status_code,
        outcome: call.outcome,
        retry_after_secs: call.retry_after_secs,
        usage: call.usage,
        turn_id: call.turn.as_ref().and_then(|turn| turn.turn_id.clone()),
        request_kind: call.turn.and_then(|turn| turn.request_kind),
    }
}
