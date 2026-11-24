#![cfg(test)]

use crate::support;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use axum::{Json, Router, routing::get};
use cli::Exit;
use support::{config, environment, home, run};
use wire::gateway::HealthzResponse;

const DEAD_PID: u32 = 2_000_000_000;

fn health(version: &'static str) -> Router {
    Router::new().route(
        "/healthz",
        get(move || async move {
            Json(HealthzResponse {
                ok: true,
                sessions: 0,
                name: "host".to_owned(),
                node: "node".to_owned(),
                degraded: None,
                degraded_providers: Vec::new(),
                version: Some(version.to_owned()),
                revision: None,
                policy: None,
            })
        }),
    )
}

async fn unused_port() -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("listener");
    listener.local_addr().expect("address").port()
}

fn state_dir(env: &config::Environment) -> std::path::PathBuf {
    let state = gateway::state_dir(env).expect("state directory");
    std::fs::create_dir_all(&state).expect("state directory");
    state
}

#[tokio::test]
async fn status_finds_a_gateway_that_bound_a_port_the_config_does_not_name() {
    let stub = testkit::stub_server(health("1.2.3")).await.expect("stub");
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!(
            "gateway:\n  host: 127.0.0.1\n  port: {}\n",
            unused_port().await
        ),
    );
    let unreached = run(&["status"], &env).await;
    assert!(
        unreached.stdout.contains("not running"),
        "{}",
        unreached.stdout
    );

    let record =
        gateway::runtime_record::publish(&state_dir(&env), stub.addr, false).expect("publish");
    let reached = run(&["status"], &env).await;
    assert!(
        reached.stdout.contains("running (v1.2.3)"),
        "{}",
        reached.stdout
    );

    drop(record);
    let gone = run(&["status"], &env).await;
    assert!(gone.stdout.contains("not running"), "{}", gone.stdout);
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn a_record_whose_process_is_gone_does_not_redirect_status() {
    let recorded = testkit::stub_server(health("0.0.1"))
        .await
        .expect("recorded");
    let configured = testkit::stub_server(health("2.0.0"))
        .await
        .expect("configured");
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!(
            "gateway:\n  host: 127.0.0.1\n  port: {}\n",
            configured.addr.port()
        ),
    );
    let state = state_dir(&env);
    let record_text = format!(
        r#"{{"pid":{DEAD_PID},"state_dir":{state:?},"host":"127.0.0.1","port":{},"tls":false}}"#,
        recorded.addr.port()
    );
    std::fs::write(gateway::runtime_record::record_path(&state), record_text).expect("record");
    let status = run(&["status"], &env).await;
    assert!(
        status.stdout.contains("running (v2.0.0)"),
        "{}",
        status.stdout
    );
}

#[tokio::test]
async fn the_environment_url_beats_the_runtime_record_and_the_config() {
    let recorded = testkit::stub_server(health("0.0.1"))
        .await
        .expect("recorded");
    let configured = testkit::stub_server(health("2.0.0"))
        .await
        .expect("configured");
    let chosen = testkit::stub_server(health("3.0.0")).await.expect("chosen");
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!(
            "gateway:\n  host: 127.0.0.1\n  port: {}\n",
            configured.addr.port()
        ),
    );
    let _record =
        gateway::runtime_record::publish(&state_dir(&env), recorded.addr, false).expect("publish");
    let env = env.with_override("RIVETHUB_GATEWAY_URL", format!("http://{}", chosen.addr));
    let status = run(&["status"], &env).await;
    assert!(
        status.stdout.contains("running (v3.0.0)"),
        "{}",
        status.stdout
    );
}

#[tokio::test]
async fn a_gateway_url_that_is_not_http_is_refused() {
    let home = home();
    let env = environment(&home).with_override("RIVETHUB_GATEWAY_URL", "ftp://127.0.0.1:1/");
    config(&home, "");
    let status = run(&["status", "--json"], &env).await;
    assert_ne!(status.exit, Exit::Ok);
    assert!(status.stderr.contains("http or https"), "{}", status.stderr);
}

type Seen = Arc<parking_lot::Mutex<Vec<(Method, String)>>>;

async fn recorder() -> (testkit::Stub, Seen) {
    let seen: Seen = Arc::default();
    let sink = Arc::clone(&seen);
    let app = Router::new().fallback(move |method: Method, uri: axum::http::Uri| {
        let sink = Arc::clone(&sink);
        async move {
            sink.lock().push((method, uri.path().to_owned()));
            StatusCode::SERVICE_UNAVAILABLE
        }
    });
    (testkit::stub_server(app).await.expect("recorder"), seen)
}

async fn raw_bundle(directory: &std::path::Path) -> String {
    let sqlite = directory.join("sender.sqlite");
    migrate::sqlite(&sqlite).expect("migrate");
    rusqlite::Connection::open(&sqlite)
        .expect("sqlite")
        .execute_batch(
            "INSERT INTO ros_conversations (id, session_key, agent, title, settings, created_at, updated_at) VALUES
               ('c1', 'claude-code:77777777-7777-7777-7777-777777777777', 'rivet-claude', 'sender title', '{}',
                '2026-10-01T09:00:00.000Z', '2026-10-01T11:00:00.000Z');
             INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, metadata, created_at) VALUES
               ('m1', 'c1', 'rivet-claude', 'claude-code', 'user', 'a prompt about quokkas',
                '{\"event_id\":\"evt-1\"}', '2026-10-01T09:00:00.000Z');",
        )
        .expect("seed");
    let out = directory.join("bundle.json");
    let sender_home = home();
    let sender = environment(&sender_home);
    let bundled = run(
        &[
            "memory",
            "bundle",
            "--out",
            &out.display().to_string(),
            "--session",
            "c1",
            "--sqlite",
            &sqlite.display().to_string(),
        ],
        &sender,
    )
    .await;
    assert_eq!(bundled.exit, Exit::Ok, "{}", bundled.stderr);
    out.display().to_string()
}

#[tokio::test]
async fn ingest_bundle_posts_to_the_gateway_the_flag_names() {
    let (flagged, flagged_seen) = recorder().await;
    let (from_env, env_seen) = recorder().await;
    let (recorded, record_seen) = recorder().await;
    let home = home();
    let directory = tempfile::tempdir().expect("directory");
    let bundle = raw_bundle(directory.path()).await;
    let env = environment(&home)
        .with_override("RIVETHUB_GATEWAY_URL", format!("http://{}", from_env.addr));
    config(&home, "");
    let _record =
        gateway::runtime_record::publish(&state_dir(&env), recorded.addr, false).expect("publish");

    let flag = format!("http://{}", flagged.addr);
    let _: cli::Report = run(
        &[
            "memory",
            "ingest-bundle",
            &bundle,
            "--force",
            "--gateway",
            &flag,
        ],
        &env,
    )
    .await;

    assert!(
        flagged_seen
            .lock()
            .iter()
            .any(|(method, _)| method == Method::POST),
        "the flag's gateway got the capture post"
    );
    assert!(
        env_seen.lock().is_empty(),
        "the environment's gateway was not used"
    );
    assert!(
        record_seen.lock().is_empty(),
        "the recorded gateway was not used"
    );

    let _: cli::Report = run(&["memory", "ingest-bundle", &bundle, "--force"], &env).await;
    assert!(
        !env_seen.lock().is_empty(),
        "without the flag the environment URL is used"
    );
    assert!(record_seen.lock().is_empty());
}
