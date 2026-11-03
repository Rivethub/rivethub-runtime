use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use kernel::topics::DeliveredVia;
use kernel::transcript::TranscriptWindow;
use wire::gateway::sessions::HarnessSessionForkRequest;
use wire::harness::{HarnessCode, HarnessSessionSummary};

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;
use crate::history::authorize;
use crate::routes::terminal::service;
use crate::tenancy::UserId;
use crate::term::ChatStart;

pub(super) async fn fork(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
    Body(request): Body<HarnessSessionForkRequest>,
) -> Result<Json<HarnessSessionSummary>, ApiError> {
    let service = service(&gateway)?;
    let resolved = gateway.history().resolve(&gateway, &segment).await?;
    authorize(&gateway, &user, &resolved).await?;
    let carried = service
        .adapter()
        .map(|adapter| adapter.system_prompt(resolved.harness).delivered_via());
    if carried.is_none_or(|via| via == DeliveredVia::None) {
        return Err(ApiError::Code(
            HarnessCode::CapabilityUnsupported,
            format!(
                "{} chats take no earlier conversation to fork from",
                resolved.harness
            ),
        ));
    }
    let window = request
        .from
        .map_or(TranscriptWindow::Latest, TranscriptWindow::From);
    let read = gateway
        .history()
        .transcript(&gateway, &resolved, window)
        .await?;
    let prompts = usize::try_from(request.prompts)
        .map_err(|_| ApiError::BadRequest("prompts is out of range".into()))?;
    let kept = kernel::fork::kept(&read.read.turns, prompts)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let source = super::described(&gateway, &resolved).await?;
    let from = source.title.as_deref().map_or_else(
        || resolved.session.to_string(),
        |title| format!("\u{201c}{title}\u{201d}"),
    );
    let start = ChatStart {
        harness: resolved.harness,
        model: source.model,
        effort: None,
        cwd: None,
        inherited_cwd: source.cwd.map(PathBuf::from),
        agent: None,
        startup_context: Some(kernel::fork::render(kept, &from)),
        topic: None,
        lineage: None,
    };
    Ok(Json(service.start_chat(&user, start).await?))
}
