use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::ConnectInfo;
use axum::extract::Path;
use axum::extract::State;
use chrono::Utc;
use kernel::harness_setup::CaptureHistory;
use kernel::harness_setup::ConnectOutcome;
use kernel::harness_setup::ConnectPart;
use kernel::harness_setup::HarnessStatus;
use kernel::harness_setup::SetupError;
use serde::Deserialize;

use crate::Gateway;
use crate::auth::is_loopback_remote;
use crate::error::ApiError;
use crate::extract::Body;
use crate::tenancy::UserId;

const LOCAL_ONLY: &str = "harness files are read and written only from the machine they are on";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectBody {
    part: ConnectPart,
    #[serde(default)]
    dry_run: bool,
}

fn local_owner(gateway: &Gateway, user: &UserId, remote: SocketAddr) -> Result<(), ApiError> {
    if !is_loopback_remote(remote) {
        return Err(ApiError::Forbidden(LOCAL_ONLY.to_owned()));
    }
    gateway.tenancy.owner(user)
}

pub(crate) async fn status(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
) -> Result<Json<Vec<HarnessStatus>>, ApiError> {
    local_owner(&gateway, &user, remote)?;
    let setup = gateway.harness_setup()?;
    let activity = gateway.store().capture_activity().await?;
    let probes = setup.probe().await.map_err(setup_error)?;
    let now = Utc::now();
    let statuses = probes
        .into_iter()
        .map(|probe| {
            let history = CaptureHistory::of(&activity, &probe.session_prefix);
            probe.into_status(history, now)
        })
        .collect();
    Ok(Json(statuses))
}

pub(crate) async fn connect(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    Body(body): Body<ConnectBody>,
) -> Result<Json<ConnectOutcome>, ApiError> {
    local_owner(&gateway, &user, remote)?;
    let outcome = gateway
        .harness_setup()?
        .connect(&id, body.part, body.dry_run)
        .await
        .map_err(setup_error)?;
    Ok(Json(outcome))
}

fn setup_error(error: SetupError) -> ApiError {
    match error {
        SetupError::UnknownHarness(_) => ApiError::Missing(error.to_string()),
        SetupError::Unsupported { .. } | SetupError::Config { .. } => {
            ApiError::Unprocessable(error.to_string())
        }
        SetupError::Io { source, .. } => ApiError::Io(source),
    }
}
