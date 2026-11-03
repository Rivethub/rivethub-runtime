use std::collections::HashSet;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;
use std::net::SocketAddr;

use axum::extract::ConnectInfo;
use axum::extract::Request;
use axum::extract::State;
use axum::http::HeaderValue;
use axum::http::Method;
use axum::http::StatusCode;
use axum::http::header;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::response::Response;
use parking_lot::Mutex;
use url::Url;

use crate::GuardState;
use crate::error::ApiError;
use crate::tenancy::PeerCertificate;

const CORS_METHODS: &str = "GET, POST, PATCH, DELETE, OPTIONS";
const CORS_HEADERS: &str = "content-type, authorization, x-rivet-conversation, x-rivet-title";
const BUILTIN_ORIGIN: &str = "app://bundle";
const DENIAL_LOG_CAP: usize = 100;

#[derive(Debug, Default)]
pub(crate) struct DenialLog {
    seen: Mutex<HashSet<String>>,
}

impl DenialLog {
    pub(crate) fn log(&self, reason: &str) {
        let first = {
            let mut seen = self.seen.lock();
            seen.len() < DENIAL_LOG_CAP && seen.insert(reason.to_owned())
        };
        if first {
            tracing::warn!("[rivethub-gateway] refused browser request: {reason}");
        }
    }
}

#[must_use]
pub fn is_loopback_remote(addr: SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(ip) => ip == Ipv4Addr::LOCALHOST,
        IpAddr::V6(ip) => {
            ip == Ipv6Addr::LOCALHOST || ip == Ipv6Addr::from([0, 0, 0, 0, 0, 0xffff, 0x7f00, 1])
        }
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost" | "::ffff:127.0.0.1")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow { echo: Option<String> },
    Deny { reason: String },
}

#[derive(Debug)]
pub struct OriginCheck<'a> {
    pub origin: Option<&'a str>,
    pub host: Option<&'a str>,
    pub caller_loopback: bool,
    pub plain_http: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginPolicy {
    allowed_origins: Vec<String>,
    allowed_hosts: Vec<String>,
}

impl OriginPolicy {
    #[must_use]
    pub fn new(allowed_origins: Vec<String>, allowed_hosts: Vec<String>) -> Self {
        Self {
            allowed_origins,
            allowed_hosts,
        }
    }

    pub fn evaluate(&self, check: &OriginCheck<'_>) -> Decision {
        if check.plain_http && check.caller_loopback {
            let host = host_header_token(check.host);
            let allowed = is_loopback_host(&host)
                || host.ends_with(".localhost")
                || self.allowed_hosts.contains(&host);
            if !allowed {
                return Decision::Deny {
                    reason: format!(
                        "host \"{host}\" is not allowed (add it to the gateway allowed_hosts setting)"
                    ),
                };
            }
        }
        let Some(raw) = check.origin.map(str::trim) else {
            return Decision::Allow { echo: None };
        };
        if raw.eq_ignore_ascii_case("null") {
            return Decision::Deny {
                reason: format!("origin \"{raw}\" is not allowed"),
            };
        }
        let Some(parsed) = normalize_origin(raw) else {
            return Decision::Deny {
                reason: format!("origin \"{raw}\" is not allowed"),
            };
        };
        if parsed == BUILTIN_ORIGIN || same_origin(&parsed, check.host) || self.listed(&parsed) {
            return Decision::Allow { echo: Some(parsed) };
        }
        Decision::Deny {
            reason: format!(
                "origin \"{parsed}\" is not allowed (add it to the gateway allowed_origins setting)"
            ),
        }
    }

    fn listed(&self, origin: &str) -> bool {
        self.allowed_origins
            .iter()
            .filter_map(|candidate| normalize_origin(candidate))
            .any(|candidate| candidate == origin)
    }
}

fn host_header_token(host: Option<&str>) -> String {
    host.map(host_token).unwrap_or_default()
}

fn same_origin(origin: &str, host_header: Option<&str>) -> bool {
    let Some(host_header) = host_header else {
        return false;
    };
    let Some((origin_host, origin_port)) = authority_of(origin) else {
        return false;
    };
    origin_host == host_token(host_header) && origin_port == header_port_value(host_header)
}

fn normalize_origin(raw: &str) -> Option<String> {
    let parsed = Url::parse(raw).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let dropped = match parsed.scheme() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    match parsed.port() {
        Some(explicit) if dropped != Some(explicit) => {
            Some(format!("{}://{host}:{explicit}", parsed.scheme()))
        }
        _ => Some(format!("{}://{host}", parsed.scheme())),
    }
}

fn authority_of(origin: &str) -> Option<(String, Option<u16>)> {
    let rest = origin.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = match authority.rsplit_once(':') {
        Some((before, after)) if !after.is_empty() && !before.contains(':') => {
            let port = after.parse::<u16>().ok()?;
            (before.to_string(), Some(port))
        }
        _ => (authority.to_string(), None),
    };
    Some((host.0.to_ascii_lowercase(), host.1))
}

fn host_token(host_header: &str) -> String {
    let trimmed = host_header.trim();
    if let Some(rest) = trimmed.strip_prefix('[') {
        let (inner, _) = rest.split_once(']').unwrap_or((rest, ""));
        inner.to_ascii_lowercase()
    } else {
        match trimmed.rsplit_once(':') {
            Some((before, after))
                if !after.is_empty() && after.bytes().all(|b| b.is_ascii_digit()) =>
            {
                before.to_ascii_lowercase()
            }
            _ => trimmed.to_ascii_lowercase(),
        }
    }
}

fn header_port_value(host_header: &str) -> Option<u16> {
    let trimmed = host_header.trim();
    let after = if trimmed.starts_with('[') {
        trimmed.split_once(']')?.1.strip_prefix(':')?
    } else {
        trimmed.rsplit_once(':')?.1
    };
    if after.is_empty() {
        return None;
    }
    after.parse().ok()
}

fn header_text<'a>(
    headers: &'a axum::http::HeaderMap,
    name: &header::HeaderName,
) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

pub(crate) async fn guard(
    State(state): State<GuardState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    mut request: Request,
    next: Next,
) -> Response {
    let caller_loopback = is_loopback_remote(addr);
    request.headers_mut().remove("x-rivetos-user");
    let path = request.uri().path().to_string();
    let origin = header_text(request.headers(), &header::ORIGIN);
    let host = header_text(request.headers(), &header::HOST);
    let check = OriginCheck {
        origin,
        host,
        caller_loopback,
        plain_http: !state.secure,
    };
    let echo = match state.gateway.policy().evaluate(&check) {
        Decision::Deny { reason } => {
            state.gateway.denials().log(&reason);
            return ApiError::ForbiddenOrigin.into_response();
        }
        Decision::Allow { echo } => echo,
    };
    if request.method() == Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        append_cors(&mut response, echo.as_deref());
        return response;
    }
    let anonymous_tls = state.secure && !state.gateway.client_certificate_required();
    let plugin = request
        .extensions()
        .get::<crate::plugins::PluginCaller>()
        .is_some();
    let authenticated = plugin
        || caller_loopback
        || anonymous_tls
        || (state.secure && request.extensions().get::<PeerCertificate>().is_some());
    let open = admits_without_client_auth(
        request.method(),
        &path,
        state.gateway.devices().enrollment_on(),
    );
    let identified = plugin || open || anonymous_tls || !state.gateway.identity_required || {
        let (parts, body) = request.into_parts();
        let resolved = state.gateway.tenancy.resolve(&parts).is_ok();
        request = Request::from_parts(parts, body);
        resolved
    };
    if !(open || authenticated && identified) {
        let mut unauthorized = unauthorized(&request, &path);
        if header_text(unauthorized.headers(), &header::CONTENT_TYPE) == Some("application/json") {
            append_cors(&mut unauthorized, echo.as_deref());
        }
        return unauthorized;
    }
    let mut response = next.run(request).await;
    append_cors(&mut response, echo.as_deref());
    response
}

fn admits_without_client_auth(method: &Method, path: &str, enrollment_on: bool) -> bool {
    matches!(
        path,
        "/healthz" | "/ca/v1/chain" | "/ca/v1/crl" | "/ca/v1/sign"
    ) || crate::routes::devices::admits_remote_without_client_auth(method, path, enrollment_on)
}

fn unauthorized(request: &Request, path: &str) -> Response {
    let method = request.method();
    let wants_html = (method == Method::GET || method == Method::HEAD)
        && !path.starts_with("/api/")
        && header_text(request.headers(), &header::ACCEPT)
            .is_some_and(|accept| accept.contains("text/html"));
    if wants_html {
        return html_unauthorized();
    }
    ApiError::Unauthorized.into_response()
}

fn html_unauthorized() -> Response {
    let body = concat!(
        "<!doctype html>\n",
        "<html>\n",
        "<head><title>RivetHub — device certificate required</title></head>\n",
        "<body>\n",
        "<h1>Device certificate required</h1>\n",
        "<p>RivetHub desktop needs a device certificate for this node. Enroll it with ",
        "<code>rh cert renew</code> and a device token from the node's operator.</p>\n",
        "<p>Liveness is at <code>/healthz</code>.</p>\n",
        "</body>\n",
        "</html>\n",
    );
    (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())],
        body,
    )
        .into_response()
}

fn append_cors(response: &mut Response, echo: Option<&str>) {
    let headers = response.headers_mut();
    if let Some(origin) = echo
        && let Ok(value) = HeaderValue::from_str(origin)
    {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(CORS_METHODS),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(CORS_HEADERS),
    );
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderMap;
    use axum::http::HeaderValue;
    use axum::http::header;

    use super::header_text;

    #[test]
    fn a_non_utf8_header_value_is_no_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_bytes(&[0xff, 0xfe]).expect("opaque bytes form a header value"),
        );
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:5174"));
        assert_eq!(header_text(&headers, &header::ORIGIN), None);
        assert_eq!(header_text(&headers, &header::HOST), Some("127.0.0.1:5174"));
        assert_eq!(header_text(&headers, &header::ACCEPT), None);
    }
}
