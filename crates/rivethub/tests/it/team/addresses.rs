use reqwest::Method;
use serde_json::Value;

use super::{Hub, PHIL, TestResult, listed, segment};

const SESSION: &str = "claude-code:00000000-0000-4000-8000-00000000b001";

fn field<'a>(body: &'a Value, pointer: &str) -> Option<&'a str> {
    body.pointer(pointer).and_then(Value::as_str)
}

#[tokio::test]
async fn every_form_of_a_session_name_reaches_the_same_session_on_every_route() -> TestResult {
    let hub = Hub::start().await?;
    hub.capture(
        PHIL,
        SESSION,
        &["phil names the session four ways today", "noted"],
    )
    .await?;
    let page = hub.get(PHIL, "/api/sessions").await?;
    assert_eq!(listed(&page), [SESSION]);
    let conversation = field(&page.body, "/items/0/conversationId")
        .ok_or("a session row carries its conversation id")?
        .to_owned();
    let forms = [
        segment(SESSION)?,
        SESSION.to_owned(),
        SESSION.replace(':', "%3A"),
        conversation.clone(),
    ];
    for form in &forms {
        let detail = hub.get(PHIL, &format!("/api/sessions/{form}")).await?;
        assert_eq!(detail.status, 200, "{form}: {}", detail.body);
        assert_eq!(field(&detail.body, "/id"), Some(SESSION), "{form}");
        assert_eq!(
            field(&detail.body, "/conversationId"),
            Some(conversation.as_str()),
            "{form}"
        );
        for (path, pointer) in [
            (format!("/api/sessions/{form}/visibility"), "/sessionId"),
            (format!("/api/memory/summaries/{form}"), "/sessionId"),
        ] {
            let reply = hub.get(PHIL, &path).await?;
            assert_eq!(reply.status, 200, "{path}: {}", reply.body);
            assert_eq!(field(&reply.body, pointer), Some(SESSION), "{path}");
        }
        for path in [
            format!("/api/sessions/{form}/placement"),
            format!("/api/sessions/{form}/model-calls"),
        ] {
            let reply = hub.get(PHIL, &path).await?;
            assert_eq!(reply.status, 200, "{path}: {}", reply.body);
        }
        for (method, path) in [
            (Method::PUT, format!("/api/sessions/{form}/keep")),
            (Method::DELETE, format!("/api/sessions/{form}/keep")),
            (Method::POST, format!("/api/sessions/{form}/trash")),
            (Method::POST, format!("/api/sessions/{form}/restore")),
            (Method::PUT, format!("/api/sessions/{form}/history")),
            (Method::DELETE, format!("/api/sessions/{form}/history")),
        ] {
            let reply = hub.call(PHIL, method.clone(), &path, None).await?;
            assert!(
                matches!(reply.status, 200 | 204),
                "{method} {path}: {} {}",
                reply.status,
                reply.body
            );
        }
    }
    let unknown = hub
        .get(PHIL, "/api/sessions/00000000-0000-4000-8000-0000000000ff")
        .await?;
    assert_eq!(unknown.status, 404, "an unknown uuid: {}", unknown.body);
    let garbled = hub.get(PHIL, "/api/sessions/not%20a%20session").await?;
    assert_eq!(garbled.status, 400, "{}", garbled.body);
    hub.stop().await
}
