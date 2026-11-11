use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use cli::Exit;
use cli::Report;

use super::Answer;
use super::Model;
use super::Node;
use super::serve;

#[tokio::test]
async fn short_tokens_that_can_name_people_or_places_are_scored() {
    let model = Model::scoring(&[]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let raw = node.raw(
        "s.json",
        &[&[
            "margot.vellacourt@initech.com",
            "salary=120k",
            "Globex",
            "[tool call] Bash",
            "42",
            "true",
        ]],
    );

    node.sanitize(&raw, &["--decide"]).await;

    let mut asked = model.asked();
    asked.sort();
    assert_eq!(
        asked,
        ["Globex", "margot.vellacourt@initech.com", "salary=120k"]
    );
    stub.close().await.expect("stub shuts down");
}

const SHAPES: [&str; 16] = [
    "203.0.113.7",
    "078-05-1120",
    "P4ssw0rd!",
    "hunter2",
    "山田太郎",
    "+15558675309",
    "buildbox.home",
    "jsmith42",
    "s3cr3t-pa55",
    "2001:db8::1",
    "Tr0ub4dor&3",
    "4111111111111111",
    "555-867-5309",
    "Z9x!k2#Lq",
    "müller",
    "sk-live-9f8e7d",
];

const IDENTIFIERS: [&str; 4] = [
    "6b0404f2-b094-90b8-6b01-a1c12a3a2107",
    "3f9a2c1be07d4e5f",
    "req_011CVzKq8RmT2vLx9WabQ7",
    "~/.claude/projects/-home-phil-work/6b0404f2-b094-90b8-6b01-a1c12a3a2107.jsonl",
];

#[tokio::test]
async fn short_sensitive_shapes_are_scored_and_identifiers_are_not() {
    let model = Model::scoring(&[]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let mut lines: Vec<&str> = SHAPES.to_vec();
    lines.extend_from_slice(&IDENTIFIERS[..2]);
    let mut messages: Vec<wire::capture::CaptureMessage> =
        lines.iter().map(|line| super::message(line)).collect();
    let first = messages.first_mut().expect("a message");
    first.metadata = Some(
        serde_json::from_str(&format!(
            r#"{{"uuid":"{}","parentUuid":"{}","requestId":"{}","session_jsonl_path":"{}","note":"jsmith42"}}"#,
            IDENTIFIERS[0], IDENTIFIERS[0], IDENTIFIERS[2], IDENTIFIERS[3]
        ))
        .expect("metadata"),
    );
    let raw = node.raw_sessions("s.json", vec![super::session(0, messages)]);

    node.sanitize(&raw, &["--decide"]).await;

    let asked = model.asked();
    let missing: Vec<&str> = SHAPES
        .iter()
        .copied()
        .filter(|shape| !asked.iter().any(|state| state == shape))
        .collect();
    assert!(missing.is_empty(), "never scored: {missing:?}");
    let identifiers: Vec<&String> = asked
        .iter()
        .filter(|state| IDENTIFIERS.contains(&state.as_str()))
        .collect();
    assert!(
        identifiers.is_empty(),
        "scored identifiers: {identifiers:?}"
    );
    stub.close().await.expect("stub shuts down");
}

async fn decide_fails(node: &Node, raw: &str) -> Report {
    let out = node.path("s.clean.json");
    let report = node
        .run(&["memory", "sanitize", raw, "--out", &out, "--decide"])
        .await;
    assert_eq!(report.exit, Exit::Failure, "{}", report.stdout);
    assert!(
        !std::path::Path::new(&out).exists(),
        "a failed decision pass wrote a bundle"
    );
    report
}

#[tokio::test]
async fn a_dead_model_fails_the_run_even_when_every_chunk_is_trivial() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("free port");
    let node = Node::new(&format!("http://{closed}"), 5);
    let raw = node.raw("s.json", &[&["42", "true"]]);

    decide_fails(&node, &raw).await;
}

#[tokio::test]
async fn the_stamp_names_the_model_and_counts_its_calls() {
    let model = Model::scoring(&[("Acme Corp", 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let raw = node.raw(
        "s.json",
        &[&[
            "Acme Corp is churning.",
            "Acme Corp is churning.",
            "all good",
        ]],
    );

    let sanitized = node.sanitize(&raw, &["--decide"]).await;

    let review = match sanitized.bundle().manifest.sanitization {
        wire::capture::Sanitization::Sanitized(stamp) => stamp.review,
        wire::capture::Sanitization::Raw => None,
    }
    .expect("review stamp");
    assert_eq!((review.model.as_str(), review.calls), ("stub-model", 2));
    assert_eq!(model.asked().len(), 2);
    stub.close().await.expect("stub shuts down");
}

#[derive(serde::Serialize)]
struct Padded {
    probabilities: [f64; 2],
    pad: String,
}

async fn misbehaving(answer: axum::Router) -> (testkit::Stub, Node, String) {
    let routes = answer.route("/v1/decide/info", axum::routing::get(super::info));
    let stub = testkit::stub_server(routes).await.expect("stub model");
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let raw = node.raw("s.json", &[&["Acme Corp is churning next quarter."]]);
    (stub, node, raw)
}

#[tokio::test]
async fn a_redirect_from_the_model_is_not_followed() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = hits.clone();
    let elsewhere = testkit::stub_server(axum::Router::new().fallback(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        async {
            axum::Json(Answer {
                probabilities: [0.5, 0.5],
            })
        }
    }))
    .await
    .expect("redirect target");
    let target = format!("http://{}/v1/decide", elsewhere.addr);
    let (stub, node, raw) = misbehaving(axum::Router::new().route(
        "/v1/decide",
        axum::routing::post(move || async move {
            (
                StatusCode::TEMPORARY_REDIRECT,
                [(axum::http::header::LOCATION, target)],
            )
                .into_response()
        }),
    ))
    .await;

    decide_fails(&node, &raw).await;

    assert_eq!(hits.load(Ordering::SeqCst), 0);
    stub.close().await.expect("stub shuts down");
    elsewhere.close().await.expect("target shuts down");
}

#[tokio::test]
async fn an_answer_over_the_size_cap_fails_the_run() {
    let (stub, node, raw) = misbehaving(axum::Router::new().route(
        "/v1/decide",
        axum::routing::post(|| async {
            axum::Json(Padded {
                probabilities: [0.5, 0.5],
                pad: "x".repeat(2 << 20),
            })
        }),
    ))
    .await;

    decide_fails(&node, &raw).await;

    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn probabilities_must_be_a_distribution() {
    for probabilities in [[0.2, 0.2], [-0.5, 1.5], [0.7, 0.7]] {
        let (stub, node, raw) = misbehaving(axum::Router::new().route(
            "/v1/decide",
            axum::routing::post(move || async move { axum::Json(Answer { probabilities }) }),
        ))
        .await;

        decide_fails(&node, &raw).await;

        stub.close().await.expect("stub shuts down");
    }
}

#[tokio::test]
async fn control_characters_from_the_model_never_reach_the_terminal() {
    let (stub, node, raw) = misbehaving(axum::Router::new().route(
        "/v1/decide",
        axum::routing::post(|| async {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "boom\u{1b}[2J\u{7}\rhidden",
            )
        }),
    ))
    .await;

    let report = decide_fails(&node, &raw).await;

    assert!(report.stderr.contains("boom"), "{}", report.stderr);
    assert!(
        !report.stderr.trim_end().chars().any(char::is_control),
        "{:?}",
        report.stderr
    );
    stub.close().await.expect("stub shuts down");
}
