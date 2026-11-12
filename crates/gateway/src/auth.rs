use std::collections::HashSet;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;
use std::net::SocketAddr;

use axum::extract::ConnectInfo;
use axum::extract::FromRequestParts;
use axum::extract::Request;
use axum::extract::State;
use axum::http::HeaderName;
use axum::http::HeaderValue;
use axum::http::Method;
use axum::http::StatusCode;
use axum::http::header;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::response::Response;
use parking_lot::Mutex;
use tower_http::set_header::SetResponseHeaderLayer;
use url::Url;

use crate::GuardState;
use crate::error::ApiError;
use crate::peer::Peer;

const EXCHANGE_PATH: &str = "/api/auth/session";
pub(crate) const CHALLENGE: &str = "Bearer realm=\"rivethub\"";
const X_REAL_IP: header::HeaderName = header::HeaderName::from_static("x-real-ip");
const X_FORWARDED: &str = "x-forwarded";
const X_FORWARDED_PREFIX: &str = "x-forwarded-";
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

pub(crate) fn names_a_forwarder(headers: &axum::http::HeaderMap) -> bool {
    headers.contains_key(header::FORWARDED)
        || headers.contains_key(X_REAL_IP)
        || headers.keys().any(|name| {
            name.as_str() == X_FORWARDED || name.as_str().starts_with(X_FORWARDED_PREFIX)
        })
}

pub(crate) fn direct_local(parts: &Parts) -> bool {
    let loopback = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(remote)| is_loopback_remote(*remote));
    let forwarded = names_a_forwarder(&parts.headers);
    let host = header_text(&parts.headers, &header::HOST)
        .map(str::to_owned)
        .or_else(|| {
            parts
                .uri
                .authority()
                .map(|authority| authority.as_str().to_owned())
        });
    loopback && !forwarded && host.is_some_and(|host| is_loopback_host(&host_token(&host)))
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
            let allowed = is_loopback_host(&host) || self.allowed_hosts.contains(&host);
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
    let peer = Peer::of(addr, request.headers(), state.gateway.trusted_proxies());
    request.extensions_mut().insert(peer);
    request.headers_mut().remove("x-rivetos-user");
    let path = request.uri().path().to_string();
    let origin = header_text(request.headers(), &header::ORIGIN);
    let host = header_text(request.headers(), &header::HOST);
    let check = OriginCheck {
        origin,
        host,
        caller_loopback: is_loopback_remote(addr),
        plain_http: !state.secure,
    };
    if let Decision::Deny { reason } = state.gateway.policy().evaluate(&check) {
        state.gateway.denials().log(&reason);
        return ApiError::ForbiddenOrigin.into_response();
    }
    let plugin = request
        .extensions()
        .get::<crate::plugins::PluginCaller>()
        .is_some();
    let secure = peer.secure(state.secure);
    let open = path == "/healthz"
        || (path == EXCHANGE_PATH && request.method() == Method::POST)
        || state.gateway.drops_open(request.method(), &path, secure);
    let refusal = if plugin || open {
        None
    } else {
        let public = crate::routes::serves_public_asset(request.method(), &path);
        let (parts, body) = request.into_parts();
        let resolved = if public {
            Ok(())
        } else {
            crate::tenancy::caller(&state.gateway, &parts).map(drop)
        };
        request = Request::from_parts(parts, body);
        refused(secure, &resolved)
    };
    if let Some(refusal) = refusal {
        return match refusal {
            Refused::InsufficientScope => ApiError::InsufficientScope.into_response(),
            Refused::Unauthorized => unauthorized(&request, &path),
        };
    }
    next.run(request).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refused {
    Unauthorized,
    InsufficientScope,
}

fn refused(secure: bool, resolved: &Result<(), ApiError>) -> Option<Refused> {
    if secure && matches!(resolved, Err(ApiError::InsufficientScope)) {
        Some(Refused::InsufficientScope)
    } else if secure && resolved.is_ok() {
        None
    } else {
        Some(Refused::Unauthorized)
    }
}

fn unauthorized(request: &Request, path: &str) -> Response {
    let refusal = if request.headers().contains_key(header::AUTHORIZATION) {
        ApiError::InvalidToken
    } else {
        ApiError::Unauthorized
    };
    let method = request.method();
    let wants_html = (method == Method::GET || method == Method::HEAD)
        && !path.starts_with("/api/")
        && header_text(request.headers(), &header::ACCEPT)
            .is_some_and(|accept| accept.contains("text/html"));
    if wants_html {
        return html_unauthorized(&refusal);
    }
    refusal.into_response()
}

fn html_unauthorized(refusal: &ApiError) -> Response {
    let body = concat!(
        "<!doctype html>\n",
        "<html>\n",
        "<head><title>RivetHub — sign-in required</title></head>\n",
        "<body>\n",
        "<h1>Sign-in required</h1>\n",
        "<p>This RivetHub node answers only a caller that presents a bearer token. ",
        "Ask the node's operator for one.</p>\n",
        "<p>Liveness is at <code>/healthz</code>.</p>\n",
        "</body>\n",
        "</html>\n",
    );
    (
        StatusCode::UNAUTHORIZED,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::WWW_AUTHENTICATE, refusal.challenge().to_owned()),
        ],
        body,
    )
        .into_response()
}

pub(crate) fn cors_layer(origins: &[String]) -> tower_http::cors::CorsLayer {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS#fallback: a configured origin that is not a valid header value lists no origin, as normalize_origin does"
        )
    )]
    let listed: Vec<HeaderValue> = origins
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(BUILTIN_ORIGIN))
        .filter_map(normalize_origin)
        .filter_map(|origin| HeaderValue::from_str(&origin).ok())
        .collect();
    let credentialed = listed.clone();
    tower_http::cors::CorsLayer::new()
        .allow_origin(tower_http::cors::AllowOrigin::list(listed))
        .allow_credentials(tower_http::cors::AllowCredentials::predicate(
            move |origin, _| credentialed.contains(origin),
        ))
        .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            HeaderName::from_static(crate::login::CSRF_HEADER),
            HeaderName::from_static("x-rivet-conversation"),
            HeaderName::from_static("x-rivet-title"),
        ])
}

pub(crate) fn harden(router: axum::Router) -> axum::Router {
    let fixed = |name: HeaderName, value: &'static str| {
        SetResponseHeaderLayer::if_not_present(name, HeaderValue::from_static(value))
    };
    router
        .layer(fixed(
            header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; script-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        ))
        .layer(fixed(header::X_CONTENT_TYPE_OPTIONS, "nosniff"))
        .layer(fixed(header::REFERRER_POLICY, "no-referrer"))
        .layer(fixed(header::X_FRAME_OPTIONS, "DENY"))
        .layer(axum::middleware::from_fn(no_store_api))
}

async fn no_store_api(request: Request, next: Next) -> Response {
    let api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    if api {
        response
            .headers_mut()
            .entry(header::CACHE_CONTROL)
            .or_insert(HeaderValue::from_static("no-store"));
    }
    response
}

pub(crate) fn csrf_exempt(parts: &Parts) -> bool {
    let path = parts.uri.path();
    (parts.method == Method::GET || parts.method == Method::HEAD)
        && (path == "/wiki" || path.starts_with("/wiki/"))
}

#[derive(Debug, Clone, Copy)]
pub struct FreshLogin;

impl FromRequestParts<std::sync::Arc<crate::Gateway>> for FreshLogin {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        gateway: &std::sync::Arc<crate::Gateway>,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(fresh(gateway, parts))
    }
}

fn fresh(gateway: &crate::Gateway, parts: &Parts) -> Result<FreshLogin, ApiError> {
    let (_, credential) = crate::tenancy::authenticate(gateway, parts)?;
    if let crate::tenancy::Credential::Session { auth_time } = credential {
        let age = chrono::Utc::now().timestamp().saturating_sub(auth_time);
        let window = gateway.browser().policy().fresh.as_secs();
        if !u64::try_from(age).is_ok_and(|age| age < window) {
            return Err(ApiError::ReauthRequired);
        }
    }
    Ok(FreshLogin)
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
