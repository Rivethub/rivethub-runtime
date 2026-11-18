mod assets;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::extract::State;
use axum::http::HeaderValue;
use axum::http::Method;
use axum::http::StatusCode;
use axum::http::header;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing;

use crate::Gateway;
use crate::error::ApiError;

pub(super) fn mount(router: Router<Arc<Gateway>>) -> Router<Arc<Gateway>> {
    router
        .route(
            "/",
            routing::get(root_get)
                .head(root_head)
                .fallback(super::not_found),
        )
        .fallback(fallback)
}

const NO_APP_PAGE: &str = "<!DOCTYPE html>\n<html lang=\"en\">\n<head><meta charset=\"utf-8\"><title>RivetHub</title></head>\n<body>\n<h1>This RivetHub node is running</h1>\n<p>The web app is not installed on this node. Run <code>rh update</code> to install it.</p>\n<p>Agents read memory over MCP. The app is in preview.</p>\n</body>\n</html>\n";

async fn root_get(State(gateway): State<Arc<Gateway>>) -> Response {
    if let Some(target) = gateway.settings().root_redirect.clone() {
        return redirect(&target).into_response();
    }
    let Some(root) = static_root(&gateway).await else {
        return no_app_page(&Method::GET);
    };
    gateway.count(crate::account::UsageEvent::AppOpen);
    assets::serve(&root, &Method::GET, "/")
        .await
        .into_response()
}

async fn root_head(State(gateway): State<Arc<Gateway>>) -> Response {
    let Some(root) = static_root(&gateway).await else {
        return no_app_page(&Method::HEAD);
    };
    assets::serve(&root, &Method::HEAD, "/")
        .await
        .into_response()
}

async fn static_root(gateway: &Gateway) -> Option<PathBuf> {
    if let Some(root) = &gateway.settings().static_dir {
        return Some(root.clone());
    }
    let link = crate::managed_web_link(&gateway.settings().state_dir);
    match managed_root(link).await {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(%error, "could not read the managed web bundle");
            None
        }
    }
}

async fn managed_root(link: PathBuf) -> std::io::Result<Option<PathBuf>> {
    let root = match tokio::fs::canonicalize(link).await {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match tokio::fs::metadata(root.join("index.html")).await {
        Ok(metadata) => Ok(metadata.is_file().then_some(root)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn no_app_page(method: &Method) -> Response {
    let body = if *method == Method::HEAD {
        axum::body::Body::empty()
    } else {
        axum::body::Body::from(NO_APP_PAGE)
    };
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    return Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_LENGTH, NO_APP_PAGE.len())
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
}

fn redirect(target: &str) -> Result<Response, ApiError> {
    let location = HeaderValue::from_str(target).map_err(|error| {
        tracing::error!("redirect location: {error}");
        ApiError::Internal
    })?;
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .body(axum::body::Body::empty())
        .map_err(|error| {
            tracing::error!("redirect response: {error}");
            ApiError::Internal
        })
}

async fn fallback(State(gateway): State<Arc<Gateway>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    if !serves_public_asset(&method, &path) {
        return super::not_found().await.into_response();
    }
    let Some(root) = static_root(&gateway).await else {
        return super::not_found().await.into_response();
    };
    assets::serve(&root, &method, &path).await.into_response()
}

pub(super) fn serves_public_asset(method: &Method, path: &str) -> bool {
    (*method == Method::GET || *method == Method::HEAD) && !static_blocked(path)
}

pub(super) fn static_blocked(path: &str) -> bool {
    matches!(
        path,
        "/event" | "/events" | "/state" | "/session" | "/ws" | "/layout" | "/mesh.json"
    ) || [
        "/api", "/wiki", "/term", "/files", "/audio", "/gateway", "/v1", "/ca",
    ]
    .into_iter()
    .any(|prefix| blocked_prefix(path, prefix))
}

fn blocked_prefix(path: &str, prefix: &str) -> bool {
    match path.strip_prefix(prefix) {
        Some("") => true,
        Some(rest) => rest.starts_with('/'),
        None => false,
    }
}
