use crate::support;

use axum::{Json, Router, routing::get};
use cli::Exit;
use serde::Serialize;
use support::{config, environment, home, run};
use wire::gateway::HealthzResponse;

#[derive(Serialize)]
struct Health {
    #[serde(flatten)]
    response: HealthzResponse,
    uptime: f64,
}

#[tokio::test]
async fn status_reads_health_and_optional_version_and_uptime() {
    let stub = testkit::stub_server(Router::new().route(
        "/healthz",
        get(|| async {
            Json(Health {
                response: HealthzResponse {
                    ok: true,
                    sessions: 0,
                    name: "host".to_owned(),
                    node: "node".to_owned(),
                    degraded: None,
                    degraded_providers: Vec::new(),
                    version: Some("1.2.3".to_owned()),
                    revision: None,
                    policy: None,
                },
                uptime: 12.5,
            })
        }),
    ))
    .await
    .expect("health route");
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!("gateway:\n  host: 0.0.0.0\n  port: {}\n", stub.addr.port()),
    );
    let status = run(&["status"], &env).await;
    assert_eq!(status.exit, Exit::Ok);
    assert_eq!(
        status.stdout,
        "RivetHub is running (v1.2.3); uptime 12.5s\n"
    );
    assert_eq!(status.stderr, "");
    stub.close().await.expect("stub shuts down");
    let unavailable = run(&["status"], &env).await;
    assert_eq!(unavailable.exit, Exit::Ok);
    assert!(unavailable.stdout.contains("not running"));
    assert!(unavailable.stderr.contains("Health unavailable"));
}

#[tokio::test]
async fn malformed_health_falls_back_with_a_diagnostic() {
    let stub = testkit::stub_server(Router::new().route("/healthz", get(|| async { "not JSON" })))
        .await
        .expect("health route");
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!(
            "gateway:\n  host: 127.0.0.1\n  port: {}\n",
            stub.addr.port()
        ),
    );
    let status = run(&["status"], &env).await;
    assert_eq!(status.exit, Exit::Ok);
    assert!(status.stderr.contains("Health unavailable"));
    assert!(status.stdout.contains("not running"));
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn ordinary_redirects_reach_the_health_route() {
    let stub = testkit::stub_server(
        Router::new()
            .route(
                "/healthz",
                get(|| async { axum::response::Redirect::temporary("/ready") }),
            )
            .route(
                "/ready",
                get(|| async {
                    Json(HealthzResponse {
                        ok: true,
                        sessions: 0,
                        name: "host".to_owned(),
                        node: "node".to_owned(),
                        degraded: None,
                        degraded_providers: Vec::new(),
                        version: None,
                        revision: None,
                        policy: None,
                    })
                }),
            ),
    )
    .await
    .expect("redirect server");
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!(
            "gateway:\n  host: 127.0.0.1\n  port: {}\n",
            stub.addr.port()
        ),
    );
    let report = run(&["status"], &env).await;
    assert_eq!(report.exit, Exit::Ok);
    assert_eq!(report.stdout, "RivetHub is running\n");
    assert_eq!(report.stderr, "");
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn stalled_health_request_times_out_after_three_seconds() {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    let reached = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let handler_reached = Arc::clone(&reached);
    let handler_release = release.clone();
    let stub = testkit::stub_server(Router::new().route(
        "/healthz",
        get(move || {
            let reached = Arc::clone(&handler_reached);
            let release = handler_release.clone();
            async move {
                reached.notify_one();
                release.cancelled().await;
                "late"
            }
        }),
    ))
    .await
    .expect("stalled server");
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!(
            "gateway:\n  host: 127.0.0.1\n  port: {}\n",
            stub.addr.port()
        ),
    );
    let request = tokio::spawn(async move { run(&["status"], &env).await });
    tokio::time::timeout(Duration::from_secs(5), reached.notified())
        .await
        .expect("request arrived");
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3)).await;
    let report = tokio::time::timeout(Duration::from_secs(1), request)
        .await
        .expect("health deadline")
        .expect("status task");
    assert_eq!(report.exit, Exit::Ok);
    assert!(report.stderr.contains("Health unavailable"));
    assert!(report.stdout.contains("not running"));
    release.cancel();
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn status_names_each_degraded_provider() {
    let stub = testkit::stub_server(Router::new().route(
        "/healthz",
        get(|| async {
            Json(HealthzResponse {
                ok: true,
                sessions: 0,
                name: "host".to_owned(),
                node: "node".to_owned(),
                degraded: None,
                degraded_providers: vec![
                    wire::gateway::DegradedProvider {
                        id: "hermes-cli".to_owned(),
                        recovery: wire::gateway::ProviderRecovery::Retrying,
                        reason: "ACP discovery timed out after 30000ms".to_owned(),
                    },
                    wire::gateway::DegradedProvider {
                        id: "kimi".to_owned(),
                        recovery: wire::gateway::ProviderRecovery::Permanent,
                        reason: "not installed".to_owned(),
                    },
                ],
                version: None,
                revision: None,
                policy: None,
            })
        }),
    ))
    .await
    .expect("health route");
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!(
            "gateway:\n  host: 127.0.0.1\n  port: {}\n",
            stub.addr.port()
        ),
    );
    let report = run(&["status"], &env).await;
    assert_eq!(report.exit, Exit::Ok);
    assert_eq!(
        report.stdout,
        "RivetHub is running\nProvider hermes-cli is degraded (retrying): ACP discovery timed out after 30000ms\nProvider kimi is degraded (permanent): not installed\n"
    );
    stub.close().await.expect("stub shuts down");
}

type Squatter = (
    u16,
    std::sync::Arc<parking_lot::Mutex<Vec<Option<String>>>>,
    tokio::task::JoinHandle<()>,
);

async fn squatter() -> Result<Squatter, std::io::Error> {
    let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let recorder = std::sync::Arc::clone(&seen);
    let app = Router::new().fallback(move |headers: axum::http::HeaderMap| {
        let recorder = std::sync::Arc::clone(&recorder);
        async move {
            recorder.lock().push(
                headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
            );
            axum::http::StatusCode::NOT_FOUND
        }
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let task = tokio::spawn(async move {
        drop(axum::serve(listener, app).await);
    });
    Ok((port, seen, task))
}

#[tokio::test]
async fn health_probes_never_carry_the_owner_token() -> Result<(), std::io::Error> {
    let (port, seen, task) = squatter().await?;
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    support::local_owner(home.path());
    run(&["status"], &env).await;
    run(&["local", "--ensure", "--port", &port.to_string()], &env).await;
    let seen = seen.lock().clone();
    assert!(!seen.is_empty(), "the probes reached the listener");
    assert!(
        seen.iter().all(Option::is_none),
        "a health probe carried a credential: {seen:?}"
    );
    task.abort();
    Ok(())
}

#[tokio::test]
async fn the_owner_sees_provider_causes_that_an_anonymous_probe_does_not() {
    let token = gateway::local_owner::OwnerToken::generate().expect("token");
    let expected = format!("Bearer {}", token.expose());
    let stub = testkit::stub_server(Router::new().route(
        "/healthz",
        get(move |headers: axum::http::HeaderMap| {
            let expected = expected.clone();
            async move {
                let owner = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    == Some(expected.as_str());
                let reason = if owner {
                    "ACP discovery timed out after 30000ms"
                } else {
                    "unavailable; see the gateway log"
                };
                Json(HealthzResponse {
                    ok: true,
                    sessions: 0,
                    name: "host".to_owned(),
                    node: "node".to_owned(),
                    degraded: None,
                    degraded_providers: vec![wire::gateway::DegradedProvider {
                        id: "hermes-cli".to_owned(),
                        recovery: wire::gateway::ProviderRecovery::Retrying,
                        reason: reason.to_owned(),
                    }],
                    version: None,
                    revision: None,
                    policy: None,
                })
            }
        }),
    ))
    .await
    .expect("health route");
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!(
            "gateway:\n  host: 127.0.0.1\n  port: {}\n",
            stub.addr.port()
        ),
    );
    let anonymous = run(&["status"], &env).await;
    assert!(
        anonymous
            .stdout
            .contains("unavailable; see the gateway log"),
        "{}",
        anonymous.stdout
    );
    support::install_owner_token(&env, &token);
    let owner = run(&["status"], &env).await;
    assert!(
        owner
            .stdout
            .contains("ACP discovery timed out after 30000ms"),
        "{}",
        owner.stdout
    );
    stub.close().await.expect("stub shuts down");
}
