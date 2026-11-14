use axum::http::StatusCode;
use wire::gateway::topics::SessionHistoryList;
use wire::gateway::topics::SessionHistoryMark;
use wire::gateway::topics::TopicRecord;
use wire::session_id::SessionId;
use wire::session_id::encode_segment;

use super::Api;
use super::TestResult;

#[tokio::test]
async fn topics_close_and_reopen_and_sessions_are_marked_as_history() -> TestResult {
    let api = Api::new(true)?;
    let created: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"Release"}"#,
            StatusCode::CREATED,
        )
        .await?;
    assert_eq!(created.closed_at, None);
    let closed: TopicRecord = api
        .json("POST", "/api/topics/release/close", "", StatusCode::OK)
        .await?;
    assert!(closed.closed_at.is_some());
    let shown: TopicRecord = api
        .json("GET", "/api/topics/release", "", StatusCode::OK)
        .await?;
    assert_eq!(shown.closed_at, closed.closed_at);
    let reopened: TopicRecord = api
        .json("POST", "/api/topics/release/reopen", "", StatusCode::OK)
        .await?;
    assert_eq!(reopened.closed_at, None);
    assert_eq!(
        api.error("POST", "/api/topics/nowhere/close", "").await?.0,
        StatusCode::NOT_FOUND
    );
    let session: SessionId = "claude-code:00000000-0000-4000-8000-000000000041".parse()?;
    let path = format!("/api/sessions/{}/history", encode_segment(&session));
    let marked: SessionHistoryMark = api.json("PUT", &path, "", StatusCode::OK).await?;
    assert_eq!(marked.session_id, session);
    let listed: SessionHistoryList = api
        .json("GET", "/api/sessions/history", "", StatusCode::OK)
        .await?;
    assert_eq!(listed.sessions, [marked]);
    for _ in 0..2 {
        assert_eq!(
            api.call("DELETE", &path, "").await?.0,
            StatusCode::NO_CONTENT
        );
    }
    let emptied: SessionHistoryList = api
        .json("GET", "/api/sessions/history", "", StatusCode::OK)
        .await?;
    assert_eq!(emptied.sessions, []);
    assert_eq!(
        api.error(
            "PUT",
            "/api/sessions/00000000-0000-4000-8000-000000000041/history",
            ""
        )
        .await?,
        (StatusCode::NOT_FOUND, "not found".to_owned())
    );
    Ok(())
}
