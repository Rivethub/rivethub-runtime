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
