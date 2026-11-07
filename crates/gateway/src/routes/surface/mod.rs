mod assets;

use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::extract::State;
use axum::http::HeaderValue;
use axum::http::Method;
use axum::http::StatusCode;
use axum::http::Uri;
use axum::http::header;
use axum::http::uri::PathAndQuery;
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

pub(crate) fn rewrite_request(mut request: Request) -> Request {
    let Some(path) = aliased(request.uri().path()) else {
        return request;
    };
    let mut raw = path;
    if let Some(query) = request.uri().query() {
        raw.push('?');
        raw.push_str(query);
    }
    let path_and_query = match raw.parse::<PathAndQuery>() {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::error!("alias path {raw}: {error}");
            return request;
        }
    };
    let mut parts = request.uri().clone().into_parts();
    parts.path_and_query = Some(path_and_query);
    let uri = match Uri::from_parts(parts) {
        Ok(uri) => uri,
        Err(error) => {
            tracing::error!("alias uri {raw}: {error}");
            return request;
        }
    };
    *request.uri_mut() = uri;
    request
}

fn aliased(path: &str) -> Option<String> {
    let exact = match path {
        "/api/events" => Some("/events"),
        "/api/events/event" => Some("/event"),
        "/api/events/sessions" => Some("/sessions"),
        "/api/events/state" => Some("/state"),
        "/api/events/ws" => Some("/ws"),
        "/api/terminal" | "/api/terminal/ws" => Some("/term"),
        "/api/terminal/config" => Some("/term/config"),
        "/api/terminal/list" => Some("/term/list"),
        "/api/terminal/inject" => Some("/term/inject"),
        "/api/terminal/harness-sessions" => Some("/term/harness-sessions"),
        _ => None,
    };
    if let Some(path) = exact {
        return Some(path.to_owned());
    }
    let rest = path.strip_prefix("/api/terminal/")?;
    Some(format!("/term/{rest}"))
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
    match path {
        "/event" | "/events" | "/sessions" | "/state" | "/session" | "/layout" => true,
        _ => {
            blocked_prefix(path, "/api")
                || blocked_prefix(path, "/term")
                || blocked_prefix(path, "/files")
                || blocked_prefix(path, "/audio")
                || blocked_prefix(path, "/gateway")
                || blocked_prefix(path, "/v1")
                || blocked_prefix(path, "/wiki")
        }
    }
}

fn blocked_prefix(path: &str, prefix: &str) -> bool {
    match path.strip_prefix(prefix) {
        Some("") => true,
        Some(rest) => rest.starts_with('/'),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::aliased;

    #[test]
    fn exact_alias_beats_the_terminal_prefix() {
        assert_eq!(aliased("/api/terminal/ws").as_deref(), Some("/term"));
        assert_eq!(aliased("/api/terminal").as_deref(), Some("/term"));
        assert_eq!(aliased("/api/terminal/list").as_deref(), Some("/term/list"));
        assert_eq!(
            aliased("/api/terminal/session/1").as_deref(),
            Some("/term/session/1")
        );
        assert_eq!(aliased("/api/events/ws").as_deref(), Some("/ws"));
        assert_eq!(aliased("/api/events").as_deref(), Some("/events"));
    }

    #[test]
    fn unknown_event_children_and_other_case_stay_put() {
        assert_eq!(aliased("/api/events/"), None);
        assert_eq!(aliased("/api/events/foo"), None);
        assert_eq!(aliased("/API/EVENTS/SESSIONS"), None);
        assert_eq!(aliased("/mesh"), None);
        assert_eq!(aliased("/api/mesh"), None);
        assert_eq!(aliased("/api/audio"), None);
        assert_eq!(aliased("/api/files/list"), None);
    }
}
