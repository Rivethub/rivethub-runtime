use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use wire::gateway::memory_models::ModelJob;

use crate::Gateway;
use crate::error::ApiError;
use crate::tenancy::UserId;

pub(super) async fn list(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
) -> Result<Json<Vec<ModelJob>>, ApiError> {
    gateway.tenancy.owner(&user)?;
    let assignments = gateway.model_assignments();
    if assignments.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let tasks = gateway
        .upkeep()?
        .tasks()
        .await
        .map_err(super::tidy::error)?;
    Ok(Json(crate::models::report(
        assignments,
        gateway.provider_health(),
        &tasks,
    )))
}
