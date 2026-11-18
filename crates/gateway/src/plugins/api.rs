use jiff::Timestamp;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use kernel::plugins::PluginAccess;
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;
use wire::capture::CaptureBatchRequest;
use wire::gateway::GatewayError;
use wire::gateway::memory::MemoryAppendToolArgs;
use wire::plugin::PluginId;
use wire::plugin::manifest::Permission;
use wire::plugin::process::{PluginNotification, PluginNotify, PluginViewsChanged};

use super::{HostError, PluginHost};
use crate::extract::Body;

const SOCKET_PATH_LIMIT: usize =
    size_of::<nix::libc::sockaddr_un>() - std::mem::offset_of!(nix::libc::sockaddr_un, sun_path);

const ATTRIBUTED_BODY_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct PluginCaller {
    pub(crate) plugin: PluginId,
}

pub(super) async fn bind(path: &Path) -> Result<UnixListener, HostError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(path).map_err(|error| {
        HostError::Unavailable(format!(
            "cannot listen on {} ({} of at most {} bytes): {error}",
            path.display(),
            path.as_os_str().len(),
            SOCKET_PATH_LIMIT - 1
        ))
    })?;
    super::registry::set_private(path).await?;
    Ok(listener)
}

pub(super) async fn serve(
    host: Arc<PluginHost>,
    inner: Router,
    listener: UnixListener,
    stop: CancellationToken,
) {
    let router = Router::new()
        .route("/api/plugin/self", get(own))
        .route("/api/plugin/views/changed", post(views_changed))
        .route("/api/plugin/notify", post(notify))
        .fallback_service(inner)
        .layer(middleware::from_fn_with_state(Arc::clone(&host), authorize))
        .with_state(host);
    if let Err(error) = axum::serve(listener, router)
        .with_graceful_shutdown(stop.cancelled_owned())
        .await
    {
        tracing::error!(%error, "plug-in API socket stopped");
    }
}

fn refusal(status: StatusCode, error: &str, code: &str, detail: Option<String>) -> Response {
    let mut body = GatewayError::new(error).code(code);
    if let Some(detail) = detail {
        body = body.detail(detail);
    }
    (status, Json(body)).into_response()
}

async fn authorize(
    State(host): State<Arc<PluginHost>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some((plugin, granted)) =
        crate::auth::bearer(request.headers()).and_then(|token| host.authenticate(token))
    else {
        return refusal(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "unauthorized",
            None,
        );
    };
    let read = matches!(*request.method(), Method::GET | Method::HEAD);
    let path = request.uri().path().to_owned();
    match kernel::plugins::access(read, &path) {
        PluginAccess::Own => {}
        PluginAccess::Needs(permission) if granted.contains(&super::rules::body(permission)) => {}
        PluginAccess::Needs(permission) => {
            return refusal(
                StatusCode::FORBIDDEN,
                "the plug-in lacks the permission this route needs",
                "permission_required",
                Some(permission.to_string()),
            );
        }
        PluginAccess::Refused => {
            return refusal(
                StatusCode::FORBIDDEN,
                "this route is not open to plug-ins",
                "permission_required",
                None,
            );
        }
    }
    request.headers_mut().remove(header::AUTHORIZATION);
    request.extensions_mut().insert(PluginCaller {
        plugin: plugin.clone(),
    });
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))));
    match attribute(request, &plugin, &granted).await {
        Ok(request) => next.run(request).await,
        Err(Unattributed::TooLarge(detail)) => refusal(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body too large",
            "body_too_large",
            Some(detail),
        ),
        Err(Unattributed::Invalid(detail)) => refusal(
            StatusCode::BAD_REQUEST,
            "invalid body",
            "invalid_body",
            Some(detail),
        ),
    }
}

enum Unattributed {
    TooLarge(String),
    Invalid(String),
}

async fn attribute(
    request: Request,
    plugin: &PluginId,
    granted: &[Permission],
) -> Result<Request, Unattributed> {
    let path = request.uri().path().to_owned();
    let capture = path == "/api/capture";
    let append = path == "/api/memory/tool/memory_append";
    if !granted.contains(&Permission::MemoryWrite) || !(capture || append) {
        return Ok(request);
    }
    let (mut parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, ATTRIBUTED_BODY_LIMIT)
        .await
        .map_err(|error| Unattributed::TooLarge(error.to_string()))?;
    let agent = format!("plugin:{}", plugin.as_ref());
    let rewritten = if capture {
        rewrite::<CaptureBatchRequest>(&bytes, |batch| {
            batch.session_key = owned_key(&agent, &batch.session_key);
            batch.agent = agent;
        })
    } else {
        rewrite::<MemoryAppendToolArgs>(&bytes, |args| {
            args.session_id = owned_key(&agent, &args.session_id);
            args.agent = Some(agent);
        })
    };
    let rewritten = rewritten.map_err(Unattributed::Invalid)?;
    parts
        .headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from(rewritten.len()));
    Ok(Request::from_parts(
        parts,
        axum::body::Body::from(rewritten),
    ))
}

fn owned_key(agent: &str, given: &str) -> String {
    let prefix = format!("{agent}:");
    if given.starts_with(&prefix) {
        given.to_owned()
    } else {
        format!("{prefix}{given}")
    }
}

fn rewrite<T>(bytes: &Bytes, stamp: impl FnOnce(&mut T)) -> Result<Vec<u8>, String>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let mut value: T = super::parse(bytes)?;
    stamp(&mut value);
    serde_json::to_vec(&value).map_err(|error| error.to_string())
}

async fn own(
    State(host): State<Arc<PluginHost>>,
    Extension(caller): Extension<PluginCaller>,
) -> Response {
    match host.own(&caller.plugin) {
        Ok(own) => Json(own).into_response(),
        Err(error) => refusal(StatusCode::NOT_FOUND, &error.to_string(), "not_found", None),
    }
}

async fn views_changed(
    State(host): State<Arc<PluginHost>>,
    Extension(caller): Extension<PluginCaller>,
    Body(changed): Body<PluginViewsChanged>,
) -> StatusCode {
    host.views_changed(&caller.plugin, changed.views);
    StatusCode::NO_CONTENT
}

async fn notify(
    State(host): State<Arc<PluginHost>>,
    Extension(caller): Extension<PluginCaller>,
    Body(notice): Body<PluginNotify>,
) -> StatusCode {
    host.notify(PluginNotification {
        plugin: caller.plugin,
        node: host.node().to_owned(),
        title: notice.title,
        text: notice.text,
        tone: notice.tone,
        action: notice.action,
        ts: Timestamp::now(),
    });
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn a_socket_path_that_is_too_long_is_named_with_its_limit() {
        let folder = tempfile::tempdir().expect("folder");
        let path = folder.path().join("s".repeat(200));
        let error = super::bind(&path).await.expect_err("too long");
        let text = error.to_string();
        assert!(text.contains(&path.display().to_string()), "{text}");
        assert!(text.contains("of at most"), "{text}");
    }
}
