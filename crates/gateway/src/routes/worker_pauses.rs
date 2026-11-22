use std::str::FromStr;
use std::sync::Arc;

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use kernel::queue::JobQueueError;
use kernel::queue::pause::PausableRole;
use kernel::tidy::TaskActivity;
use wire::gateway::worker_pauses::WorkerPause;
use wire::gateway::worker_pauses::WorkerPauseUpdate;
use wire::gateway::worker_pauses::WorkerPauses;

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;
use crate::tenancy::UserId;

pub(super) async fn list(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
) -> Result<Json<WorkerPauses>, ApiError> {
    gateway.tenancy.owner(&user)?;
    let paused = gateway.worker_pauses()?.paused().await.map_err(error)?;
    let tasks = queued(&gateway).await?;
    Ok(Json(crate::worker_pauses::listed(&paused, &tasks)))
}

pub(super) async fn set(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
    Body(update): Body<WorkerPauseUpdate>,
) -> Result<Json<WorkerPause>, ApiError> {
    gateway.tenancy.owner(&user)?;
    let role = PausableRole::from_str(&segment)
        .map_err(|unpausable| ApiError::BadRequest(unpausable.to_string()))?;
    let pauses = gateway.worker_pauses()?;
    let pause = if update.paused {
        Some(pauses.pause(role, user.as_str()).await.map_err(error)?)
    } else {
        pauses.resume(role).await.map_err(error)?;
        None
    };
    let tasks = queued(&gateway).await?;
    Ok(Json(crate::worker_pauses::status(
        role,
        pause.as_ref(),
        &tasks,
    )))
}

async fn queued(gateway: &Gateway) -> Result<Vec<TaskActivity>, ApiError> {
    gateway.upkeep()?.tasks().await.map_err(super::tidy::error)
}

fn error(error: JobQueueError) -> ApiError {
    if let JobQueueError::Unavailable(source) = error {
        tracing::error!("worker pauses failed: {source}");
        return ApiError::Unavailable("worker pauses unavailable".into());
    }
    tracing::error!("worker pauses failed: {error}");
    ApiError::Internal
}
