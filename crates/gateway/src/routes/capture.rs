use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::request::Parts;
use wire::capture::CaptureResult;

use crate::Gateway;
use crate::error::ApiError;
use crate::routes::capture_schema::ValidatedBatch;

pub(crate) async fn ingest(
    State(gateway): State<Arc<Gateway>>,
    parts: Parts,
    ValidatedBatch(mut batch): ValidatedBatch,
) -> Result<Json<CaptureResult>, ApiError> {
    let (user, credential) = crate::tenancy::authenticate(&gateway, &parts)?;
    if !credential.is_local_owner() && batch.drop_receipt() {
        tracing::debug!(
            "dropped a settings.receipt from a caller without the local owner credential"
        );
    }
    let session_key = batch.session_key.clone();
    let captured = gateway
        .store()
        .capture(
            crate::memory_body::capture_batch_request(batch),
            user.owner(),
            Some(&gateway.settings().node),
        )
        .await?;
    Ok(Json(CaptureResult {
        ok: true,
        conversation_id: session_key,
        inserted: captured.inserted,
        skipped: captured.skipped,
    }))
}
