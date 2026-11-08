use wire::model_call::{ModelCallBatchResult, SessionModelCalls};

use super::{TestResult, start, stop};

const BATCH: &str = r#"{"calls":[
  {"call_id":"00000000-0000-4000-8000-0000000000e1","started_at":"2026-11-08T12:00:00Z","finished_at":"2026-11-08T12:00:02Z","provider":"anthropic","model_requested":"alias","model_served":"model-a","account":"work","status_code":200,"outcome":"completed","usage":{"input":10,"output":4,"cache_read":100,"cache_write":7,"reasoning":0},"headers":{"X-Claude-Code-Session-Id":"00000000-0000-4000-8000-0000000000c1","User-Agent":"claude-cli/2.1.319 (external, cli)"}},
  {"call_id":"00000000-0000-4000-8000-0000000000e2","started_at":"2026-11-08T12:01:00Z","provider":"openai","model_requested":"model-b","status_code":429,"outcome":"rate_limited","retry_after_secs":30,"headers":{"session-id":"00000000-0000-7000-8000-0000000000c2","user-agent":"codex_exec/0.167.0","x-codex-turn-metadata":"{\"turn_id\":\"00000000-0000-7000-8000-0000000000d1\",\"request_kind\":\"turn\"}"}},
  {"call_id":"00000000-0000-4000-8000-0000000000e3","started_at":"2026-11-08T12:02:00Z","provider":"anthropic","outcome":"aborted","headers":{}}
]}"#;

#[tokio::test]
async fn local_stores_model_calls_per_session_and_refuses_credential_headers() -> TestResult {
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let port_text = port.to_string();
    let started = start(
        home.path(),
        &["local", "--no-lan", "--port", &port_text],
        &["gateway listening on "],
    )
    .await?;
    let client = reqwest::Client::builder().no_proxy().build()?;
    let base = format!("http://127.0.0.1:{port}");
    let outcome = exercise(&client, &base).await;
    stop(started).await?;
    outcome
}

async fn exercise(client: &reqwest::Client, base: &str) -> TestResult {
    let post = |body: &'static str| {
        client
            .post(format!("{base}/api/capture/model-calls"))
            .header("content-type", "application/json")
            .body(body)
            .send()
    };
    let first = post(BATCH).await?;
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let first: ModelCallBatchResult = first.json().await?;
    assert_eq!(
        (
            first.inserted,
            first.duplicate,
            first.matched,
            first.unmatched
        ),
        (3, 0, 2, 1)
    );
    let repeat: ModelCallBatchResult = post(BATCH).await?.json().await?;
    assert_eq!((repeat.inserted, repeat.duplicate), (0, 3));

    let claude: SessionModelCalls = client
        .get(format!(
            "{base}/api/sessions/claude-code:00000000-0000-4000-8000-0000000000c1/model-calls"
        ))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!((claude.totals.calls, claude.totals.errors), (1, 0));
    assert_eq!(claude.totals.usage.cache_write, 7);
    let served = claude
        .calls
        .first()
        .and_then(|call| call.model_served.as_deref());
    assert_eq!(served, Some("model-a"));

    let codex: SessionModelCalls = client
        .get(format!(
            "{base}/api/sessions/codex:00000000-0000-7000-8000-0000000000c2/model-calls"
        ))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!((codex.totals.calls, codex.totals.errors), (1, 1));
    let kind = codex
        .calls
        .first()
        .and_then(|call| call.request_kind.as_deref());
    assert_eq!(kind, Some("turn"));

    let credential = post(
        r#"{"calls":[{"call_id":"00000000-0000-4000-8000-0000000000e9","started_at":"2026-11-08T12:00:00Z","provider":"p","outcome":"completed","headers":{"Authorization":"Bearer not-a-real-secret"}}]}"#,
    )
    .await?;
    assert_eq!(credential.status(), reqwest::StatusCode::BAD_REQUEST);
    let body = credential.text().await?;
    assert!(body.contains("authorization"), "{body}");
    assert!(!body.contains("not-a-real-secret"), "{body}");
    Ok(())
}
