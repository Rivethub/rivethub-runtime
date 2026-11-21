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
    let tasks = gateway
        .upkeep()?
        .tasks()
        .await
        .map_err(super::tidy::error)?;
    if let Some(reporting) = gateway.worker_reports() {
        let reports = reporting
            .store
            .newest_first(kernel::tidy::REPORT_FRESH)
            .await
            .map_err(super::tidy::error)?;
        if !reports.is_empty() || !assignments.is_empty() {
            return Ok(Json(crate::worker_models::reported(
                &reports,
                &reporting.hub,
                &tasks,
            )));
        }
    }
    if assignments.is_empty() {
        return Ok(Json(crate::models::unconfigured(&tasks)));
    }
    Ok(Json(crate::models::report(
        assignments,
        gateway.provider_health(),
        &tasks,
    )))
}
