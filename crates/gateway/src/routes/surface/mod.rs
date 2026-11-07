mod assets;

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

async fn root_get(State(gateway): State<Arc<Gateway>>) -> Response {
    if let Some(target) = gateway.settings().root_redirect.clone() {
        return redirect(&target).into_response();
    }
    gateway.count(crate::account::UsageEvent::AppOpen);
    assets::serve(&gateway, &Method::GET, "/")
        .await
        .into_response()
}

async fn root_head(State(gateway): State<Arc<Gateway>>) -> Response {
    assets::serve(&gateway, &Method::HEAD, "/")
        .await
        .into_response()
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
    if method != Method::GET && method != Method::HEAD {
        return super::not_found().await.into_response();
    }
    assets::serve(&gateway, &method, &path)
        .await
        .into_response()
}

pub(super) fn static_blocked(path: &str) -> bool {
    blocked_prefix(path, "/api") || blocked_prefix(path, "/wiki")
}

fn blocked_prefix(path: &str, prefix: &str) -> bool {
    match path.strip_prefix(prefix) {
        Some("") => true,
        Some(rest) => rest.starts_with('/'),
        None => false,
    }
}
