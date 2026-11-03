use std::str::FromStr;
use std::sync::Arc;

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::Utc;
use kernel::flagged::FlaggedError;
use kernel::flagged::FlaggedGroup;
use kernel::flagged::FlaggedId;
use kernel::flagged::FlaggedJob;
use wire::gateway::flagged::FlaggedGroup as FlaggedGroupBody;
use wire::gateway::flagged::FlaggedJob as FlaggedJobBody;
use wire::gateway::flagged::FlaggedJobs;

use crate::Gateway;
use crate::error::ApiError;
use crate::tenancy::UserId;

pub(super) async fn list(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
) -> Result<Json<FlaggedJobs>, ApiError> {
    gateway.tenancy.owner(&user)?;
    let jobs = gateway.flagged()?.flagged().await.map_err(error)?;
    Ok(Json(FlaggedJobs {
        groups: kernel::flagged::group(jobs)
            .into_iter()
            .map(group)
            .collect(),
    }))
}

pub(super) async fn retry(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<StatusCode, ApiError> {
    gateway.tenancy.owner(&user)?;
    let id = flagged_id(&segment)?;
    gateway.flagged()?.retry(id).await.map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn dismiss(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<StatusCode, ApiError> {
    gateway.tenancy.owner(&user)?;
    let id = flagged_id(&segment)?;
    gateway
        .flagged()?
        .dismiss(id, Utc::now())
        .await
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn flagged_id(segment: &str) -> Result<FlaggedId, ApiError> {
    FlaggedId::from_str(segment)
        .map_err(|_| ApiError::BadRequest(format!("{segment} is not a flagged job id")))
}

fn error(error: FlaggedError) -> ApiError {
    match error {
        FlaggedError::Missing(_) => ApiError::Missing(error.to_string()),
        FlaggedError::Invalid { .. } => {
            tracing::error!("{error}");
            ApiError::Internal
        }
        FlaggedError::Unavailable(source) => {
            tracing::error!("flagged jobs failed: {source}");
            ApiError::Unavailable("flagged jobs unavailable".into())
        }
    }
}

fn group(group: FlaggedGroup) -> FlaggedGroupBody {
    FlaggedGroupBody {
        task: group.task,
        cause: group.cause,
        count: group.count,
        last_failed_at: group.last_failed_at,
        sample: job(group.sample),
    }
}

fn job(job: FlaggedJob) -> FlaggedJobBody {
    FlaggedJobBody {
        id: i64::from(job.id),
        task: job.task,
        key: job.key,
        subject: job.subject,
        payload: job.payload,
        error: job.error,
        failures: job.failures,
        first_failed_at: job.first_failed_at,
        last_failed_at: job.last_failed_at,
        flagged_at: job.flagged_at,
    }
}
