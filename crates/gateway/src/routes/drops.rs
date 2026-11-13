use std::sync::Arc;

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::body::{Body, BodyDataStream};
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing;
use futures::StreamExt as _;
use tokio_util::io::ReaderStream;
use wire::gateway::drops::{DropRefusal, Uploaded};
use wire::gateway::users::TokenScope;

use crate::Gateway;
use crate::drops::store::{Claim, Refused, Reservation, ReserveError};
use crate::drops::{DropId, Drops, RetryAfter, SEND_DEADLINE, UPLOAD_DEADLINE};
use crate::peer::Peer;
use crate::tenancy::UserId;

const DRAIN_SLACK: u64 = 1 << 20;

pub(crate) fn router(drops: Arc<Drops>) -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/api/drops",
            routing::post(upload).fallback(method_not_allowed),
        )
        .route(
            "/api/drops/{id}",
            routing::get(download)
                .head(method_not_allowed)
                .delete(revoke)
                .fallback(method_not_allowed),
        )
        .layer(axum::middleware::map_response(no_store))
        .layer(Extension(drops))
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn refusal(status: StatusCode, error: &str) -> Response {
    (
        status,
        Json(DropRefusal {
            error: error.to_owned(),
            status: status.as_u16(),
        }),
    )
        .into_response()
}

fn not_found() -> Response {
    refusal(StatusCode::NOT_FOUND, "not found")
}

fn internal() -> Response {
    refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
}

fn limited(wait: RetryAfter) -> Response {
    let mut response = refusal(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded");
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(wait.seconds()));
    response
}

const NO_TOKEN: &str = r#"Bearer realm="rivethub", scope="drops:write""#;
const INVALID_TOKEN: &str =
    r#"Bearer realm="rivethub", error="invalid_token", scope="drops:write""#;
const INSUFFICIENT_SCOPE: &str =
    r#"Bearer realm="rivethub", error="insufficient_scope", scope="drops:write""#;

fn challenged(status: StatusCode, error: &str, challenge: &'static str) -> Response {
    let mut response = refusal(status, error);
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static(challenge),
    );
    response
}

async fn method_not_allowed(request: Request) -> Response {
    drain(&mut request.into_body().into_data_stream(), 0).await;
    refusal(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a stated rule, as note 143 of the contract treats an authorization header that is not text as no token, which refuses the upload with 401"
        )
    )]
    let text = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = text.trim().split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

fn holder(gateway: &Gateway, headers: &HeaderMap) -> Result<UserId, Turned> {
    if !headers.contains_key(header::AUTHORIZATION) {
        return Err(Turned::NoToken);
    }
    let (user, scope) = bearer(headers)
        .and_then(|token| gateway.tenancy.user_for_token(token))
        .ok_or(Turned::InvalidToken)?;
    if scope.allows(TokenScope::DropsWrite) {
        Ok(user)
    } else {
        Err(Turned::InsufficientScope)
    }
}

async fn drain(stream: &mut BodyDataStream, cap: u64) {
    let mut seen: u64 = 0;
    let read = async {
        while let Some(Ok(chunk)) = stream.next().await {
            seen += chunk.len() as u64;
            if seen > cap {
                return;
            }
        }
    };
    if tokio::time::timeout(UPLOAD_DEADLINE, read).await.is_err() {
        tracing::debug!("a refused drop upload was still sending at its deadline");
    }
}

fn declared_length(headers: &HeaderMap) -> Option<u64> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a stated rule, as note 143 of the contract reads a content-length that is not a number as none declared, so the upload reserves max_bytes"
        )
    )]
    let length: u64 = headers
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()?;
    Some(length)
}

#[derive(Debug)]
enum Turned {
    NoToken,
    InvalidToken,
    InsufficientScope,
    TooLarge,
    Limited(RetryAfter),
    Quota,
    Internal,
}

impl IntoResponse for Turned {
    fn into_response(self) -> Response {
        match self {
            Self::NoToken => challenged(StatusCode::UNAUTHORIZED, "invalid_token", NO_TOKEN),
            Self::InvalidToken => {
                challenged(StatusCode::UNAUTHORIZED, "invalid_token", INVALID_TOKEN)
            }
            Self::InsufficientScope => challenged(
                StatusCode::FORBIDDEN,
                "insufficient_scope",
                INSUFFICIENT_SCOPE,
            ),
            Self::TooLarge => refusal(StatusCode::PAYLOAD_TOO_LARGE, "payload too large"),
            Self::Limited(wait) => limited(wait),
            Self::Quota => refusal(StatusCode::TOO_MANY_REQUESTS, "drop quota exceeded"),
            Self::Internal => internal(),
        }
    }
}

fn admit<'a>(
    gateway: &Gateway,
    drops: &'a Drops,
    headers: &HeaderMap,
) -> Result<Reservation<'a>, Turned> {
    let user = holder(gateway, headers)?;
    let declared = declared_length(headers);
    if declared.is_some_and(|length| length > drops.max_bytes()) {
        return Err(Turned::TooLarge);
    }
    drops.upload(&user).map_err(Turned::Limited)?;
    let reserve = declared
        .filter(|length| *length >= 1)
        .unwrap_or_else(|| drops.max_bytes());
    drops
        .store()
        .reserve(&user, reserve)
        .map_err(|refused| match refused {
            ReserveError::Quota => Turned::Quota,
            ReserveError::Random(error) => {
                tracing::warn!(kind = ?error.kind(), "drop id could not be drawn");
                Turned::Internal
            }
        })
}

async fn upload(
    State(gateway): State<Arc<Gateway>>,
    Extension(drops): Extension<Arc<Drops>>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let mut stream = body.into_data_stream();
    let cap = drops.max_bytes() + DRAIN_SLACK;
    let reservation = match admit(&gateway, &drops, &parts.headers) {
        Ok(reservation) => reservation,
        Err(turned) => {
            drain(&mut stream, cap).await;
            return turned.into_response();
        }
    };
    let received = tokio::time::timeout(
        UPLOAD_DEADLINE,
        drops.store().receive(reservation, &mut stream),
    )
    .await
    .map_err(|_| Refused::Deadline)
    .and_then(|stored| stored);
    if matches!(received, Err(Refused::NotAge | Refused::TooLarge)) {
        drain(&mut stream, cap).await;
    }
    match received {
        Ok(stored) => (
            StatusCode::CREATED,
            Json(Uploaded {
                id: stored.id.as_str().to_owned(),
                expires_at: stored.expires_at,
                bytes: stored.bytes,
            }),
        )
            .into_response(),
        Err(Refused::NotAge) => refusal(StatusCode::BAD_REQUEST, "body is not an age file"),
        Err(Refused::TooLarge) => refusal(StatusCode::PAYLOAD_TOO_LARGE, "payload too large"),
        Err(Refused::Aborted) => refusal(StatusCode::BAD_REQUEST, "upload aborted"),
        Err(Refused::Io(error)) => {
            tracing::warn!(op = "upload", kind = ?error.kind(), "drop file operation failed");
            internal()
        }
        Err(Refused::Deadline) => refusal(StatusCode::REQUEST_TIMEOUT, "upload deadline passed"),
    }
}

fn drop_id(segment: Result<Path<String>, PathRejection>) -> Option<DropId> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a stated rule, as note 143 of the contract answers a malformed drop id, invalid UTF-8 included, with the same 404 as an unknown one"
        )
    )]
    let id = segment.ok()?.0.parse().ok()?;
    Some(id)
}

async fn download(
    Extension(drops): Extension<Arc<Drops>>,
    Extension(peer): Extension<Peer>,
    segment: Result<Path<String>, PathRejection>,
) -> Response {
    let ip = peer.client();
    if let Err(wait) = drops.download(ip) {
        return limited(wait);
    }
    let gone = || {
        drops.missed(ip);
        not_found()
    };
    let Some(id) = drop_id(segment) else {
        return gone();
    };
    let owned = Arc::clone(&drops);
    let claimed = tokio::task::spawn_blocking(move || owned.store().claim(&id)).await;
    let (file, bytes) = match claimed {
        Ok(Claim::Ready { file, bytes }) => (file, bytes),
        Ok(Claim::Gone) => return gone(),
        Ok(Claim::Failed) => return internal(),
        Err(error) => {
            tracing::error!("drop claim task failed: {error}");
            return internal();
        }
    };
    let stream = ReaderStream::new(tokio::fs::File::from_std(file))
        .take_until(tokio::time::sleep(SEND_DEADLINE));
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            ),
            (header::CONTENT_LENGTH, HeaderValue::from(bytes)),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

async fn revoke(
    State(gateway): State<Arc<Gateway>>,
    Extension(drops): Extension<Arc<Drops>>,
    segment: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Response {
    let user = match holder(&gateway, &headers) {
        Ok(user) => user,
        Err(turned) => return turned.into_response(),
    };
    let Some(id) = drop_id(segment) else {
        return not_found();
    };
    let owned = Arc::clone(&drops);
    let revoked = tokio::task::spawn_blocking(move || owned.store().revoke(&id, &user)).await;
    match revoked {
        Ok(Ok(true)) => StatusCode::NO_CONTENT.into_response(),
        Ok(Ok(false)) => not_found(),
        Ok(Err(error)) => {
            tracing::warn!(op = "revoke", kind = ?error.kind(), "drop file operation failed");
            internal()
        }
        Err(error) => {
            tracing::error!("drop revoke task failed: {error}");
            internal()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse as _;

    use super::Turned;

    #[tokio::test]
    async fn a_token_without_drops_write_gets_the_rfc_6750_403() {
        let response = Turned::InsufficientScope.into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.headers().get(header::WWW_AUTHENTICATE),
            Some(&header::HeaderValue::from_static(
                r#"Bearer realm="rivethub", error="insufficient_scope", scope="drops:write""#
            ))
        );
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("a refusal body");
        assert_eq!(&body[..], br#"{"error":"insufficient_scope","status":403}"#);
    }
}
