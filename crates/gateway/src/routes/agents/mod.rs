mod body;
mod changes;
mod check;
mod error;
mod paths;
mod registry;
mod store;

use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::Path as AgentId;
use axum::extract::Request;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::error::Category;
use wire::JsonObject;
use wire::gateway::OkReply;
use wire::gateway::agents::AgentListResponse;
use wire::gateway::agents::AgentPresetCreateRequest;
use wire::gateway::agents::AgentPresetUpdate;
use wire::gateway::agents::AgentResponse;

use crate::Gateway;
use crate::error::ApiError;
use crate::error::body_must_be_object;
use crate::error::invalid_json;
use crate::extract::Query;
use crate::extract::limited_bytes;
use crate::tenancy::UserId;

use error::AgentError;
use kernel::agent_presets::AgentPresetStore;
use store::FilePresets;

pub(crate) use registry::Registry as AgentRegistry;

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/",
            routing::get(list)
                .post(create)
                .fallback(super::method_not_allowed),
        )
        .route(
            "/{id}",
            routing::get(get_one)
                .patch(patch)
                .delete(remove)
                .fallback(super::method_not_allowed),
        )
        .fallback(super::not_found)
        .layer(DefaultBodyLimit::max(super::BODY_LIMIT))
}

pub(crate) fn open(settings: &crate::Settings) -> AgentRegistry {
    AgentRegistry::new(
        settings,
        Arc::new(FilePresets::new(settings.state_dir.clone())),
    )
}

pub(crate) fn backed_by(
    settings: &crate::Settings,
    store: Arc<dyn AgentPresetStore>,
) -> AgentRegistry {
    AgentRegistry::new(settings, store)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ListQuery {
    node: Option<String>,
}

pub(crate) enum Failure {
    Api(ApiError),
    Agent(AgentError),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(error) => error.fmt(formatter),
            Self::Agent(error) => error.fmt(formatter),
        }
    }
}

impl From<ApiError> for Failure {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl From<AgentError> for Failure {
    fn from(error: AgentError) -> Self {
        Self::Agent(error)
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        match self {
            Self::Api(error) => error.into_response(),
            Self::Agent(error) => error.into_response(),
        }
    }
}

async fn list(
    State(gateway): State<Arc<Gateway>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<AgentListResponse>, Failure> {
    let agents = gateway.presets().list(query.node).await?;
    Ok(Json(AgentListResponse {
        agents,
        node: gateway.settings().node.clone(),
        directory_root: Some(path_text(&gateway.settings().directory_root)?),
        shared_dir: Some(path_text(&gateway.settings().shared_dir)?),
        backend: Some(gateway.presets().backend()),
    }))
}

async fn create(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    request: Request,
) -> Result<Response, Failure> {
    gateway.tenancy.owner(&user)?;
    let body: AgentPresetCreateRequest = read_json(request).await?;
    let agent = gateway.presets().create(body).await?;
    Ok((StatusCode::CREATED, Json(AgentResponse { agent })).into_response())
}

async fn get_one(
    State(gateway): State<Arc<Gateway>>,
    AgentId(id): AgentId<String>,
) -> Result<Json<AgentResponse>, Failure> {
    let agent = gateway.presets().get(id).await?;
    Ok(Json(AgentResponse { agent }))
}

async fn patch(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    AgentId(id): AgentId<String>,
    request: Request,
) -> Result<Json<AgentResponse>, Failure> {
    gateway.tenancy.owner(&user)?;
    let body: AgentPresetUpdate = read_json(request).await?;
    let agent = gateway.presets().update(id, body).await?;
    Ok(Json(AgentResponse { agent }))
}

async fn remove(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    AgentId(id): AgentId<String>,
) -> Result<Json<OkReply>, Failure> {
    gateway.tenancy.owner(&user)?;
    gateway.presets().delete(id).await?;
    Ok(Json(OkReply { ok: true }))
}

async fn read_json<T: DeserializeOwned>(request: Request) -> Result<T, Failure> {
    let bytes = limited_bytes(request, &()).await?;
    if let Err(error) = serde_json::from_slice::<JsonObject>(&bytes) {
        return Err(shape_failure(&error));
    }
    match serde_json::from_slice(&bytes) {
        Ok(value) => Ok(value),
        Err(error) => Err(field_failure(&error)),
    }
}

fn shape_failure(error: &serde_json::Error) -> Failure {
    if error.classify() == Category::Data {
        body_must_be_object().into()
    } else {
        invalid_json().into()
    }
}

fn field_failure(error: &serde_json::Error) -> Failure {
    if error.classify() == Category::Data {
        AgentError::BadRequestOwned(strip_location(&error.to_string())).into()
    } else {
        invalid_json().into()
    }
}

fn strip_location(message: &str) -> String {
    match message.split_once(" at line ") {
        Some((head, _)) => head.to_owned(),
        None => message.to_owned(),
    }
}

fn path_text(path: &Path) -> Result<String, AgentError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(AgentError::Unavailable)
}
