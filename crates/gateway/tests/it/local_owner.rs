use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::fs::symlink;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Method;
use axum::http::Request;
use axum::http::StatusCode;
use gateway::local_owner::LocalOwner;
use gateway::local_owner::TokenError;
use gateway::local_owner::read_token;
use gateway::local_owner::token_path;
use tower::ServiceExt as _;

use crate::common;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn mode_of(path: &std::path::Path) -> TestResult<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

#[test]
fn the_token_file_is_created_once_with_owner_only_mode_and_reused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let state = dir.path().join("state");
    let first = LocalOwner::load_or_create(&state)?;
    let path = token_path(&state);
    assert_eq!(mode_of(&path)?, 0o600);
    let written = std::fs::read_to_string(&path)?;
    assert_eq!(written, first.current().expose());
    assert_eq!(written.len(), 43);
    let second = LocalOwner::load_or_create(&state)?;
    assert_eq!(second.current(), first.current());
    assert_eq!(
        read_token(&state)?.map(|token| token.expose().to_owned()),
        Some(written)
    );
    Ok(())
}

#[test]
fn a_missing_file_reads_as_no_token_and_no_file_is_made_by_reading() -> TestResult {
    let dir = tempfile::tempdir()?;
    assert!(read_token(dir.path())?.is_none());
    assert!(!token_path(dir.path()).exists());
    Ok(())
}

#[test]
fn a_symlinked_token_file_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("elsewhere");
    std::fs::write(&target, "x")?;
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))?;
    symlink(&target, token_path(dir.path()))?;
    assert!(matches!(
        LocalOwner::load_or_create(dir.path()),
        Err(TokenError::Io { .. })
    ));
    assert!(read_token(dir.path()).is_err());
    Ok(())
}

#[test]
fn a_group_or_world_readable_token_file_is_refused() -> TestResult {
    for mode in [0o640, 0o604, 0o644, 0o660] {
        let dir = tempfile::tempdir()?;
        let owner = LocalOwner::load_or_create(dir.path())?;
        let path = token_path(dir.path());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
        assert!(
            matches!(
                LocalOwner::load_or_create(dir.path()),
                Err(TokenError::Shared { .. })
            ),
            "{mode:o}"
        );
        assert!(read_token(dir.path()).is_err());
        drop(owner);
    }
    Ok(())
}

#[test]
fn a_directory_in_place_of_the_file_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir(token_path(dir.path()))?;
    assert!(matches!(
        LocalOwner::load_or_create(dir.path()),
        Err(TokenError::NotRegular { .. })
    ));
    Ok(())
}

#[test]
fn a_token_file_that_is_not_32_base64url_bytes_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = token_path(dir.path());
    std::fs::write(&path, "short")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    assert!(matches!(
        LocalOwner::load_or_create(dir.path()),
        Err(TokenError::Malformed { .. })
    ));
    Ok(())
}

#[test]
fn rotation_replaces_the_file_atomically_and_the_old_token_stops_verifying() -> TestResult {
    let dir = tempfile::tempdir()?;
    let owner = LocalOwner::load_or_create(dir.path())?;
    let old = owner.current();
    assert!(owner.verify(old.expose()));
    owner.rotate()?;
    let fresh = owner.current();
    assert_ne!(fresh, old);
    assert!(!owner.verify(old.expose()));
    assert!(owner.verify(fresh.expose()));
    let path = token_path(dir.path());
    assert_eq!(mode_of(&path)?, 0o600);
    assert_eq!(std::fs::read_to_string(&path)?, fresh.expose());
    let leftovers = std::fs::read_dir(dir.path())?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(leftovers, 0);
    Ok(())
}

#[test]
fn verify_refuses_empty_prefix_and_extended_tokens() -> TestResult {
    let owner = LocalOwner::unpersisted()?;
    let token = owner.current();
    assert!(!owner.verify(""));
    assert!(!owner.verify(&token.expose()[..42]));
    assert!(!owner.verify(&format!("{}x", token.expose())));
    Ok(())
}

#[test]
fn debug_never_prints_the_token() -> TestResult {
    let owner = LocalOwner::unpersisted()?;
    let token = owner.current();
    assert!(!format!("{token:?}").contains(token.expose()));
    assert!(!format!("{owner:?}").contains(token.expose()));
    Ok(())
}

fn route_paths(router: &axum::Router) -> Vec<String> {
    let debug = format!("{router:?}");
    let Some((_, nodes)) = debug.split_once("node: Node { paths: {") else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    let mut rest = nodes;
    while let Some(start) = rest.find("): \"") {
        let after = &rest[start + 4..];
        let Some(end) = after.find('"') else { break };
        paths.push(after[..end].to_owned());
        rest = &after[end..];
        if rest.starts_with("\"}") {
            break;
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn concrete(path: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for ch in path.chars() {
        match ch {
            '{' => {
                inside = true;
                out.push('x');
            }
            '}' => inside = false,
            _ if !inside => out.push(ch),
            _ => {}
        }
    }
    out
}

const METHODS: [Method; 4] = [Method::GET, Method::POST, Method::PATCH, Method::DELETE];
const OPEN_ROUTES: [&str; 4] = ["/", "/healthz", "/login", "/api/auth/session"];

async fn call(
    app: &axum::Router,
    method: &Method,
    path: &str,
    peer: &str,
    headers: &[(&str, &str)],
) -> TestResult<StatusCode> {
    let mut builder = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let mut request = builder.body(Body::empty())?;
    request
        .extensions_mut()
        .insert(ConnectInfo(peer.parse::<std::net::SocketAddr>()?));
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        app.clone().oneshot(request),
    )
    .await??;
    Ok(response.status())
}

const LOCAL: &str = "127.0.0.1:41000";
const HOST: &str = "127.0.0.1:5174";

fn gateway_router(secure: bool) -> axum::Router {
    let (gateway, _store) = common::gateway();
    gateway.router(secure)
}

#[test]
fn the_route_table_is_found_in_the_router() {
    let paths = route_paths(&gateway_router(false));
    assert!(paths.len() > 100, "{} routes", paths.len());
    assert!(paths.iter().any(|path| path == "/api/capture"));
    assert!(paths.iter().any(|path| path == "/healthz"));
    assert!(paths.iter().any(|path| path == "/wiki/{slug}"));
}

#[tokio::test]
async fn every_route_refuses_a_loopback_caller_with_no_credential() -> TestResult {
    let app = gateway_router(false);
    for route in route_paths(&app) {
        if OPEN_ROUTES.contains(&route.as_str()) {
            continue;
        }
        let path = concrete(&route);
        for method in &METHODS {
            let status = call(&app, method, &path, LOCAL, &[("host", HOST)]).await?;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {route}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn every_route_refuses_the_owner_token_behind_a_forwarder() -> TestResult {
    let app = gateway_router(false);
    let bearer = format!("Bearer {}", common::owner_token());
    let forwarded = [
        vec![("host", "gateway.example.test")],
        vec![("host", HOST), ("x-forwarded-for", "203.0.113.9")],
        vec![("host", HOST), ("x-forwarded-host", "gateway.example.test")],
        vec![("host", HOST), ("forwarded", "for=203.0.113.9")],
        vec![("host", HOST), ("x-real-ip", "203.0.113.9")],
    ];
    for route in route_paths(&app) {
        if OPEN_ROUTES.contains(&route.as_str()) {
            continue;
        }
        let path = concrete(&route);
        for headers in &forwarded {
            let mut with = headers.clone();
            with.push(("authorization", bearer.as_str()));
            let status = call(&app, &Method::GET, &path, LOCAL, &with).await?;
            assert!(
                matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN),
                "{route} {headers:?}: {status}"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_forwarded_owner_token_is_unauthorized_on_the_api() -> TestResult {
    let app = gateway_router(false);
    let bearer = format!("Bearer {}", common::owner_token());
    for headers in [
        vec![("host", HOST), ("x-forwarded-for", "203.0.113.9")],
        vec![("host", HOST), ("x-forwarded-proto", "https")],
        vec![("host", HOST), ("forwarded", "for=203.0.113.9")],
        vec![("host", HOST), ("x-real-ip", "203.0.113.9")],
    ] {
        let mut with = headers.clone();
        with.push(("authorization", bearer.as_str()));
        let status = call(&app, &Method::GET, "/api/memory/stats", LOCAL, &with).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{headers:?}");
    }
    Ok(())
}

#[tokio::test]
async fn the_owner_token_needs_a_loopback_host() -> TestResult {
    let app = gateway_router(true);
    let bearer = format!("Bearer {}", common::owner_token());
    for host in [
        "gateway.example.test",
        "192.0.2.7:5174",
        "127.0.0.1.evil.test",
    ] {
        let status = call(
            &app,
            &Method::GET,
            "/api/memory/stats",
            LOCAL,
            &[("host", host), ("authorization", &bearer)],
        )
        .await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{host}");
    }
    for host in ["127.0.0.1", "127.0.0.1:9", "localhost:5174", "[::1]:5174"] {
        let status = call(
            &app,
            &Method::GET,
            "/api/memory/stats",
            LOCAL,
            &[("host", host), ("authorization", &bearer)],
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{host}");
    }
    Ok(())
}

#[tokio::test]
async fn the_owner_token_is_refused_from_a_non_loopback_peer_even_over_tls() -> TestResult {
    let app = gateway_router(true);
    let bearer = format!("Bearer {}", common::owner_token());
    let status = call(
        &app,
        &Method::GET,
        "/api/memory/stats",
        "192.0.2.7:41000",
        &[("host", HOST), ("authorization", &bearer)],
    )
    .await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn every_route_accepts_the_owner_token_from_a_direct_local_caller() -> TestResult {
    let app = gateway_router(false);
    let bearer = format!("Bearer {}", common::owner_token());
    for route in route_paths(&app) {
        let path = concrete(&route);
        let status = call(
            &app,
            &Method::GET,
            &path,
            LOCAL,
            &[("host", HOST), ("authorization", &bearer)],
        )
        .await?;
        assert_ne!(status, StatusCode::UNAUTHORIZED, "{route}");
    }
    Ok(())
}

#[tokio::test]
async fn the_bearer_scheme_is_case_insensitive_and_a_wrong_scheme_is_refused() -> TestResult {
    let app = gateway_router(false);
    let token = common::owner_token();
    for scheme in ["Bearer", "bearer", "BEARER", "bEaReR"] {
        let value = format!("{scheme} {token}");
        let status = call(
            &app,
            &Method::GET,
            "/api/memory/stats",
            LOCAL,
            &[("host", HOST), ("authorization", &value)],
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{scheme}");
    }
    for value in [
        format!("Basic {token}"),
        token.clone(),
        "Bearer ".to_owned(),
    ] {
        let status = call(
            &app,
            &Method::GET,
            "/api/memory/stats",
            LOCAL,
            &[("host", HOST), ("authorization", &value)],
        )
        .await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
    }
    Ok(())
}

#[tokio::test]
async fn a_refusal_carries_the_bearer_challenge() -> TestResult {
    let app = gateway_router(false);
    let mut request = Request::builder()
        .uri("/api/memory/stats")
        .header("host", HOST)
        .body(Body::empty())?;
    request
        .extensions_mut()
        .insert(ConnectInfo(LOCAL.parse::<std::net::SocketAddr>()?));
    let response = app.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer realm=\"rivethub\"")
    );
    Ok(())
}

async fn healthz_body(
    app: &axum::Router,
    headers: &[(&str, &str)],
) -> TestResult<(StatusCode, String)> {
    let mut builder = Request::builder().uri("/healthz").header("host", HOST);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let mut request = builder.body(Body::empty())?;
    request
        .extensions_mut()
        .insert(ConnectInfo(LOCAL.parse::<std::net::SocketAddr>()?));
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20).await?;
    Ok((status, String::from_utf8(body.to_vec())?))
}

#[tokio::test]
async fn healthz_stays_open_and_hides_the_database_detail_from_an_anonymous_caller() -> TestResult {
    let app = common::failing_gateway().router(false);
    let (status, text) = healthz_body(&app, &[]).await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        text.contains("\"degraded\":\"database unreachable\""),
        "{text}"
    );
    assert!(!text.contains("down"), "{text}");
    let bearer = format!("Bearer {}", common::owner_token());
    let (_, detailed) = healthz_body(&app, &[("authorization", &bearer)]).await?;
    assert!(
        detailed.contains("database unreachable: down"),
        "{detailed}"
    );
    Ok(())
}

#[tokio::test]
async fn the_shell_and_static_assets_load_without_a_credential() -> TestResult {
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join("index.html"), "<html>shell</html>")?;
    std::fs::write(root.path().join("login.js"), "1")?;
    let mut settings = common::settings();
    settings.static_dir = Some(root.path().to_path_buf());
    let app = common::gateway_with(settings).router(false);
    for path in ["/", "/login", "/login.js", "/some/deep/link"] {
        let status = call(&app, &Method::GET, path, LOCAL, &[("host", HOST)]).await?;
        assert_eq!(status, StatusCode::OK, "{path}");
    }
    let status = call(
        &app,
        &Method::GET,
        "/api/memory/stats",
        LOCAL,
        &[("host", HOST)],
    )
    .await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn account_and_harness_routes_need_a_direct_local_caller_and_a_credential() -> TestResult {
    let (gateway, _store) = common::gateway();
    let app = gateway.router(false);
    let bearer = format!("Bearer {}", common::owner_token());
    for path in ["/api/capture/status", "/api/account"] {
        let status = call(&app, &Method::GET, path, LOCAL, &[("host", HOST)]).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }
    let _unused: Arc<gateway::Gateway> = gateway;
    let status = call(
        &app,
        &Method::GET,
        "/api/capture/status",
        LOCAL,
        &[("host", HOST), ("authorization", &bearer)],
    )
    .await?;
    assert_ne!(status, StatusCode::UNAUTHORIZED);
    assert_ne!(status, StatusCode::FORBIDDEN);
    Ok(())
}
