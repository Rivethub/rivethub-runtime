use crate::common;
use crate::session_support::user;

use std::sync::Arc;

use gateway::Gateway;
use gateway::sessions::SessionRegistry;
use gateway::tenancy::Tenancy;
use testkit::FakeMemoryStore;
use testkit::FakeModelCallStore;
use testkit::Request;
use wire::gateway::GatewayError;
use wire::model_call::{ModelCallBatchResult, SessionModelCalls};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const COCO: &str = "coco-model-call-token";
const CLAUDE_NATIVE: &str = "00000000-0000-4000-8000-0000000000c1";
const CODEX_NATIVE: &str = "00000000-0000-7000-8000-0000000000c2";

fn request(method: &str, target: &str, body: &str, token: Option<&str>) -> Request {
    let mut headers = vec![
        ("Host".to_owned(), "127.0.0.1:5174".to_owned()),
        ("Content-Type".to_owned(), "application/json".to_owned()),
    ];
    if let Some(token) = token {
        headers.push(("Authorization".to_owned(), format!("Bearer {token}")));
    }
    Request {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
        body: body.as_bytes().to_vec(),
        recipe: None,
        via: None,
    }
}

fn batch(headers_of_second: &str) -> String {
    format!(
        r#"{{"calls":[
  {{"call_id":"00000000-0000-4000-8000-0000000000e1","started_at":"2026-11-08T12:00:00Z","provider":"anthropic","model_served":"model-a","outcome":"completed","usage":{{"input":10,"output":4,"cache_read":100}},"headers":{{"X-Claude-Code-Session-Id":"{CLAUDE_NATIVE}","User-Agent":"claude-cli/2.1.319 (external, cli)"}}}},
  {{"call_id":"00000000-0000-4000-8000-0000000000e2","started_at":"2026-11-08T12:01:00Z","provider":"openai","model_requested":"model-b","status_code":429,"outcome":"rate_limited","retry_after_secs":30,"headers":{headers_of_second}}},
  {{"call_id":"00000000-0000-4000-8000-0000000000e3","started_at":"2026-11-08T12:02:00Z","provider":"anthropic","outcome":"aborted","headers":{{}}}}
]}}"#
    )
}

fn codex_headers() -> String {
    format!(
        r#"{{"session-id":"{CODEX_NATIVE}","user-agent":"codex_exec/0.167.0","x-codex-turn-metadata":"{{\"turn_id\":\"00000000-0000-7000-8000-0000000000d1\",\"request_kind\":\"turn\"}}"}}"#
    )
}

fn served() -> Result<(Arc<Gateway>, Arc<FakeModelCallStore>), Box<dyn std::error::Error>> {
    let calls = Arc::new(FakeModelCallStore::default());
    let tenancy = Tenancy::local(user("phil")).with_token(COCO, user("coco"))?;
    let gateway = Gateway::new(
        Arc::new(FakeMemoryStore::default()) as _,
        common::settings(),
    )
    .with_local_owner(crate::common::local_owner())
    .with_tenancy(tenancy)
    .with_sessions(Arc::new(
        SessionRegistry::default().with_node_owner(user("phil")),
    ))
    .with_model_calls(Arc::clone(&calls) as _);
    Ok((Arc::new(gateway), calls))
}

#[tokio::test]
async fn a_batch_is_stored_for_its_matched_sessions_and_a_repeat_changes_nothing() -> TestResult {
    let (gateway, calls) = served()?;
    let app = gateway.router(false);
    let body = batch(&codex_headers());

    let first = common::answer(
        app.clone(),
        request("POST", "/api/capture/model-calls", &body, None),
    )
    .await?;
    assert_eq!(
        first.status,
        200,
        "{}",
        String::from_utf8_lossy(&first.body)
    );
    let result: ModelCallBatchResult = serde_json::from_slice(&first.body)?;
    assert_eq!(
        (
            result.inserted,
            result.duplicate,
            result.matched,
            result.unmatched
        ),
        (3, 0, 2, 1)
    );

    let repeat = common::answer(
        app.clone(),
        request("POST", "/api/capture/model-calls", &body, None),
    )
    .await?;
    let result: ModelCallBatchResult = serde_json::from_slice(&repeat.body)?;
    assert_eq!(
        (result.inserted, result.duplicate, result.matched),
        (0, 3, 2)
    );
    assert_eq!(calls.stored().len(), 3);
    assert!(calls.stored().iter().all(|(owner, _)| owner == "phil"));

    let read = common::answer(
        app.clone(),
        request(
            "GET",
            &format!("/api/sessions/codex:{CODEX_NATIVE}/model-calls"),
            "",
            None,
        ),
    )
    .await?;
    assert_eq!(read.status, 200, "{}", String::from_utf8_lossy(&read.body));
    let session: SessionModelCalls = serde_json::from_slice(&read.body)?;
    assert_eq!(session.calls.len(), 1);
    let call = session.calls.first().ok_or("no call")?;
    assert_eq!(
        call.turn_id.as_deref(),
        Some("00000000-0000-7000-8000-0000000000d1")
    );
    assert_eq!(call.request_kind.as_deref(), Some("turn"));
    assert_eq!((session.totals.calls, session.totals.errors), (1, 1));
    assert_eq!(session.totals.models.get("model-b"), Some(&1));

    let claude = common::answer(
        app,
        request(
            "GET",
            &format!("/api/sessions/claude-code:{CLAUDE_NATIVE}/model-calls"),
            "",
            None,
        ),
    )
    .await?;
    let session: SessionModelCalls = serde_json::from_slice(&claude.body)?;
    assert_eq!((session.totals.calls, session.totals.errors), (1, 0));
    assert_eq!(
        (
            session.totals.usage.input,
            session.totals.usage.output,
            session.totals.usage.cache_read
        ),
        (10, 4, 100)
    );
    Ok(())
}

#[tokio::test]
async fn a_batch_that_holds_a_credential_header_is_refused_whole() -> TestResult {
    for (name, value) in [
        ("Authorization", "Bearer not-a-real-credential"),
        ("x-api-key", "not-a-real-credential"),
        ("Cookie", "session=not-a-real-secret"),
        ("ChatGPT-Account-Id", "not-a-real-secret"),
        ("x-refresh-token", "not-a-real-secret"),
        ("X-Client-Secret", "not-a-real-secret"),
    ] {
        let (gateway, calls) = served()?;
        let headers = format!(
            r#"{{"user-agent":"codex_exec/0.167.0","session-id":"{CODEX_NATIVE}","{name}":"{value}"}}"#
        );
        let response = common::answer(
            gateway.router(false),
            request("POST", "/api/capture/model-calls", &batch(&headers), None),
        )
        .await?;
        assert_eq!(response.status, 400, "{name}");
        let text = String::from_utf8_lossy(&response.body).into_owned();
        let message = serde_json::from_slice::<GatewayError>(&response.body)?.error;
        assert!(message.contains(&name.to_ascii_lowercase()), "{message}");
        assert!(message.contains("calls[1]"), "{message}");
        assert!(
            !text.contains(value),
            "the refusal must not echo the value: {text}"
        );
        assert!(calls.stored().is_empty(), "{name}: nothing is stored");
    }
    Ok(())
}

#[tokio::test]
async fn a_malformed_batch_is_refused_with_its_position() -> TestResult {
    let (gateway, calls) = served()?;
    let app = gateway.router(false);
    let too_big = i128::from(i64::MAX) + 1;
    for (body, needle) in [
        (
            r#"{"calls":[{"call_id":" ","started_at":"2026-11-08T12:00:00Z","provider":"p","outcome":"completed"}]}"#.to_owned(),
            "calls[0]: call_id must not be empty",
        ),
        (
            format!(
                r#"{{"calls":[{{"call_id":"c","started_at":"2026-11-08T12:00:00Z","provider":"p","outcome":"completed","usage":{{"input":{too_big}}}}}]}}"#
            ),
            "calls[0]: a count exceeds",
        ),
        (
            r#"{"calls":[{"call_id":"c","started_at":"2026-11-08T12:00:00Z","provider":"p","outcome":"exploded"}]}"#.to_owned(),
            "unknown variant",
        ),
        (r#"{"calls":[{"call_id":"c"}]}"#.to_owned(), "missing field"),
        ("not json".to_owned(), ""),
    ] {
        let response = common::answer(
            app.clone(),
            request("POST", "/api/capture/model-calls", &body, None),
        )
        .await?;
        assert_eq!(response.status, 400, "{body}");
        let message = serde_json::from_slice::<GatewayError>(&response.body)?.error;
        assert!(message.contains(needle), "{message}");
    }
    assert_eq!(calls.stored().len(), 0);
    Ok(())
}

#[tokio::test]
async fn a_session_owned_by_someone_else_is_not_found_and_the_route_needs_a_store() -> TestResult {
    let (gateway, _calls) = served()?;
    gateway.sessions().claim(&user("phil"), CLAUDE_NATIVE)?;
    let app = gateway.router(false);
    let target = format!("/api/sessions/claude-code:{CLAUDE_NATIVE}/model-calls");
    let theirs = common::answer(app.clone(), request("GET", &target, "", Some(COCO))).await?;
    assert_eq!(theirs.status, 404);
    let mine = common::answer(app, request("GET", &target, "", None)).await?;
    assert_eq!(mine.status, 200);

    let bare = Arc::new(
        Gateway::new(
            Arc::new(FakeMemoryStore::default()) as _,
            common::settings(),
        )
        .with_local_owner(crate::common::local_owner())
        .with_tenancy(Tenancy::local(user("phil"))),
    );
    let absent = common::answer(
        bare.router(false),
        request("POST", "/api/capture/model-calls", &batch("{}"), None),
    )
    .await?;
    assert_eq!(absent.status, 503);
    Ok(())
}
