#![cfg(test)]

use crate::support;

use axum::{Json, Router, routing::get};
use cli::{Exit, Report};
use config::Environment;
use support::{environment, home, run};
use tempfile::TempDir;
use wire::gateway::HealthzResponse;

fn healthy() -> HealthzResponse {
    HealthzResponse {
        ok: true,
        sessions: 0,
        name: "host".to_owned(),
        node: "node".to_owned(),
        degraded: None,
        degraded_providers: Vec::new(),
        version: Some("1.2.3".to_owned()),
        revision: None,
        policy: None,
    }
}

fn local_config(home: &TempDir, port: u16, extra: &str) -> Environment {
    let state = home.path().join(".rivethub");
    std::fs::create_dir_all(&state).expect("state directory");
    let database = state.join("memory.sqlite");
    migrate::sqlite(&database).expect("migrate");
    let text = format!(
        "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\ngateway:\n  host: 127.0.0.1\n  port: {port}\n{extra}",
        database.display()
    );
    std::fs::write(state.join("config.yaml"), text).expect("config");
    environment(home)
}

async fn status_and_doctor(port: u16, extra: &str) -> (Report, Report) {
    let home = home();
    let env = local_config(&home, port, extra);
    (run(&["status"], &env).await, run(&["doctor"], &env).await)
}

#[tokio::test]
async fn status_and_doctor_accept_the_config_rh_local_writes() {
    let stub =
        testkit::stub_server(Router::new().route("/healthz", get(|| async { Json(healthy()) })))
            .await
            .expect("health route");
    let (status, doctor) = status_and_doctor(stub.addr.port(), "").await;
    stub.close().await.expect("stub shuts down");
    assert_eq!(status.exit, Exit::Ok);
    assert_eq!(status.stdout, "RivetHub is running (v1.2.3)\n");
    assert!(!status.stderr.contains("unavailable"), "{}", status.stderr);
    assert_eq!(doctor.exit, Exit::Ok, "{}", doctor.stdout);
    assert!(doctor.stdout.contains("pass: config"), "{}", doctor.stdout);
    assert!(doctor.stdout.contains("pass: memory"), "{}", doctor.stdout);
    assert!(!doctor.stdout.contains("fail:"), "{}", doctor.stdout);
    assert!(
        !status.stderr.contains("memory.models"),
        "{}",
        status.stderr
    );
    assert!(
        !status.stdout.contains("memory.models"),
        "{}",
        status.stdout
    );
}

#[tokio::test]
async fn doctor_says_once_that_no_model_is_configured() {
    let home = home();
    let env = local_config(&home, 1, "");
    let status = run(&["status"], &env).await;
    let validate = run(&["config", "validate"], &env).await;
    let doctor = run(&["doctor"], &env).await;
    let quiet = format!("{}{}", status.stderr, validate.stderr);
    assert!(!quiet.contains("memory.models"), "{quiet}");
    assert!(!quiet.contains("No model is configured"), "{quiet}");
    assert_eq!(validate.exit, Exit::Ok, "{}", validate.stdout);
    assert_eq!(doctor.exit, Exit::Ok, "{}", doctor.stdout);
    assert!(
        doctor
            .stdout
            .contains(&format!("pass: models — {}", config::NO_MODEL_NOTE)),
        "{}",
        doctor.stdout
    );
    assert!(
        !doctor.stderr.contains("memory.models"),
        "{}",
        doctor.stderr
    );

    let configured = support::home();
    let with_agent = local_config(
        &configured,
        1,
        "runtime:\n  workspace: /tmp/work\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://127.0.0.1:1/v1\n    model: m\n",
    );
    let doctor = run(&["doctor"], &with_agent).await;
    assert_eq!(doctor.exit, Exit::Ok, "{}", doctor.stdout);
    assert!(!doctor.stdout.contains("pass: models"), "{}", doctor.stdout);
    assert!(
        !doctor.stdout.contains("No model is configured"),
        "{}",
        doctor.stdout
    );
}

#[tokio::test]
async fn status_and_doctor_still_report_broken_agents_and_providers() {
    for (extra, expected) in [
        (
            "agents: {}\n",
            "\"agents\" is empty — define at least one agent",
        ),
        (
            "providers:\n  p:\n    type: openai-compatible\n    model: m\n",
            "Provider \"p\" requires \"base_url\"",
        ),
    ] {
        let (status, doctor) = status_and_doctor(1, extra).await;
        assert!(
            status.stderr.contains("Health unavailable"),
            "{}",
            status.stderr
        );
        assert!(status.stderr.contains(expected), "{}", status.stderr);
        assert_eq!(doctor.exit, Exit::Failure, "{}", doctor.stdout);
        assert!(doctor.stdout.contains("fail: config"), "{}", doctor.stdout);
        assert!(doctor.stdout.contains(expected), "{}", doctor.stdout);
    }
}

#[tokio::test]
async fn validate_holds_a_postgres_config_to_what_rh_start_requires() {
    let home = home();
    let state = home.path().join(".rivethub");
    std::fs::create_dir_all(&state).expect("state directory");
    std::fs::write(
        state.join("config.yaml"),
        "memory:\n  backend: postgres\n  postgres:\n    url: postgres://rh@127.0.0.1:1/rh\ngateway:\n  host: 127.0.0.1\n  port: 1\n",
    )
    .expect("config");
    let validate = run(&["config", "validate"], &environment(&home)).await;
    assert_eq!(validate.exit, Exit::Failure, "{}", validate.stdout);
    for section in ["runtime", "agents", "providers"] {
        assert!(
            validate
                .stdout
                .contains(&format!("Missing required section \"{section}\"")),
            "{}",
            validate.stdout
        );
    }
}

#[tokio::test]
async fn status_and_doctor_state_the_hub_policy() {
    let stub = testkit::stub_server(Router::new().route(
        "/healthz",
        get(|| async {
            Json(HealthzResponse {
                policy: Some(wire::gateway::HubPolicy {
                    cloud: wire::gateway::CloudPolicy::Disabled,
                    share_servers: vec![
                        "team.example.com".to_owned(),
                        "rivethub.cloud:443".to_owned(),
                    ],
                }),
                ..healthy()
            })
        }),
    ))
    .await
    .expect("health route");
    let home = home();
    let env = local_config(&home, stub.addr.port(), "cloud:\n  disabled: true\n");
    let status = run(&["status"], &env).await;
    stub.close().await.expect("stub shuts down");
    assert_eq!(
        status.stdout,
        "RivetHub is running (v1.2.3)\nHub policy: cloud disabled; sharing with only team.example.com; cloud.disabled refuses rivethub.cloud:443\n"
    );
    let doctor = run(&["doctor"], &env).await;
    assert!(
        doctor
            .stdout
            .contains("pass: policy — cloud off, telemetry off, sharing with no share server"),
        "{}",
        doctor.stdout
    );

    let (_, open) = status_and_doctor(1, "").await;
    assert!(
        open.stdout
            .contains("pass: policy — cloud on, telemetry on, sharing with any share server"),
        "{}",
        open.stdout
    );
}

#[tokio::test]
async fn doctor_warns_when_systemd_would_ignore_the_environment_file() {
    let home = home();
    let env = local_config(&home, 1, "");
    let unit_dir = home.path().join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir).expect("unit dir");
    let unit = unit_dir.join("rivethub.service");
    let env_file = home.path().join(".rivethub/.env");
    std::fs::write(
        &unit,
        format!("EnvironmentFile=-\"{}\"\n", env_file.display()),
    )
    .expect("quoted unit");
    let doctor = run(&["doctor"], &env).await;
    assert_eq!(doctor.exit, Exit::Ok, "{}", doctor.stdout);
    assert!(doctor.stdout.contains("warn: service"), "{}", doctor.stdout);
    assert!(
        doctor.stdout.contains("systemd ignores"),
        "{}",
        doctor.stdout
    );
    std::fs::write(&unit, format!("EnvironmentFile=-{}\n", env_file.display()))
        .expect("absolute unit");
    let doctor = run(&["doctor"], &env).await;
    assert_eq!(doctor.exit, Exit::Ok, "{}", doctor.stdout);
    assert!(
        !doctor.stdout.contains("systemd ignores"),
        "{}",
        doctor.stdout
    );
    assert!(
        !doctor.stdout.contains("warn: service"),
        "{}",
        doctor.stdout
    );
}

#[tokio::test]
async fn doctor_fails_a_share_server_list_entry_that_is_not_a_host() {
    let home = home();
    let env = local_config(&home, 1, "");
    let path = home.path().join(".rivethub/config.yaml");
    let text = std::fs::read_to_string(&path).expect("config");
    std::fs::write(
        &path,
        text.replacen(
            "memory:\n",
            "memory:\n  share:\n    allowed_servers: [team.example.com, \"ftp://team.example.com\"]\n",
            1,
        ),
    )
    .expect("config");
    let doctor = run(&["doctor"], &env).await;
    assert_eq!(doctor.exit, Exit::Failure, "{}", doctor.stdout);
    assert!(
        doctor
            .stdout
            .contains("fail: policy — memory.share.allowed_servers: \"ftp://team.example.com\""),
        "{}",
        doctor.stdout
    );
}

#[tokio::test]
async fn ensure_no_cloud_refuses_a_running_node_that_lacks_the_policy() {
    let stub =
        testkit::stub_server(Router::new().route("/healthz", get(|| async { Json(healthy()) })))
            .await
            .expect("health route");
    let home = home();
    let env = local_config(&home, stub.addr.port(), "cloud:\n  disabled: true\n");
    let report = run(&["local", "--ensure", "--no-cloud"], &env).await;
    stub.close().await.expect("stub shuts down");
    assert_eq!(report.exit, Exit::Failure, "{}", report.stdout);
    assert!(
        report
            .stderr
            .contains("is running without this hub's cloud policy; restart it"),
        "{}",
        report.stderr
    );
}

#[tokio::test]
async fn ensure_accepts_a_node_whose_policy_has_a_shape_it_does_not_inspect() {
    let stub = testkit::stub_server(Router::new().route(
        "/healthz",
        get(|| async {
            r#"{"ok":true,"sessions":0,"name":"host","node":"node","policy":{"cloud":{"mode":"disabled"}}}"#
        }),
    ))
    .await
    .expect("health route");
    let home = home();
    let env = local_config(&home, stub.addr.port(), "");
    let report = run(&["local", "--ensure"], &env).await;
    stub.close().await.expect("stub shuts down");
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(report.stdout.contains("is running at"), "{}", report.stdout);
}

#[tokio::test]
async fn ensure_no_cloud_accepts_a_running_node_under_the_policy_even_when_degraded() {
    let stub = testkit::stub_server(Router::new().route(
        "/healthz",
        get(|| async {
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthzResponse {
                    ok: false,
                    policy: Some(wire::gateway::HubPolicy {
                        cloud: wire::gateway::CloudPolicy::Disabled,
                        share_servers: Vec::new(),
                    }),
                    ..healthy()
                }),
            )
        }),
    ))
    .await
    .expect("health route");
    let home = home();
    let env = local_config(&home, stub.addr.port(), "cloud:\n  disabled: true\n");
    let report = run(&["local", "--ensure", "--no-cloud"], &env).await;
    stub.close().await.expect("stub shuts down");
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(report.stdout.contains("is running at"), "{}", report.stdout);
}
