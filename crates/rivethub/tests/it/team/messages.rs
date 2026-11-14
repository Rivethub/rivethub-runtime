use serde_json::Value;
use wire::gateway::session_catalog::SessionMessagesPage;
use wire::message::Role;

use super::{ALEX, Hub, PHIL, TOM, TestResult, message, segment};

const ALEXS: &str = "claude-code:00000000-0000-4000-8000-00000000c001";
const TOMS: &str = "claude-code:00000000-0000-4000-8000-00000000c002";
const RECEIVED: &str = "import:00000000000000000000000000000000000000000000000000000000000000c3";

async fn page(
    hub: &Hub,
    person: &str,
    session: &str,
    query: &str,
) -> Result<SessionMessagesPage, Box<dyn std::error::Error>> {
    let reply = hub
        .get(person, &format!("/api/sessions/{session}/messages{query}"))
        .await?;
    assert_eq!(reply.status, 200, "{person} {session}: {}", reply.body);
    Ok(serde_json::from_value(reply.body)?)
}

#[tokio::test]
async fn a_session_s_messages_come_in_order_with_tool_rows_paged_and_counted() -> TestResult {
    let hub = Hub::start().await?;
    hub.share_by_default(ALEX).await?;
    let mut tool = message(ALEXS, 2, Role::Tool, "Cargo.toml\nsrc")?;
    tool.tool_name = Some("Bash".to_owned());
    tool.tool_result = Some("Cargo.toml\nsrc".to_owned());
    let rows = vec![
        message(ALEXS, 0, Role::User, "list the files")?,
        message(ALEXS, 1, Role::Assistant, "running ls")?,
        tool,
        message(ALEXS, 3, Role::Assistant, "two entries")?,
        message(ALEXS, 4, Role::User, "thanks")?,
    ];
    hub.capture_messages(ALEX, ALEXS, rows).await?;
    hub.capture(TOM, TOMS, &["tom's private notes", "ok"])
        .await?;

    let first = page(&hub, TOM, &segment(ALEXS)?, "?limit=2").await?;
    assert_eq!(first.total, 5);
    assert_eq!(first.items.len(), 2);
    let cursor = first.next.clone().ok_or("a second page")?;
    let second = page(&hub, TOM, ALEXS, &format!("?limit=2&cursor={cursor}")).await?;
    let cursor = second.next.clone().ok_or("a third page")?;
    let third = page(&hub, TOM, ALEXS, &format!("?limit=2&cursor={cursor}")).await?;
    assert_eq!(third.next, None);
    let all: Vec<_> = [first, second, third]
        .into_iter()
        .flat_map(|page| page.items)
        .collect();
    let contents: Vec<&str> = all.iter().map(|row| row.content.as_str()).collect();
    assert_eq!(
        contents,
        [
            "list the files",
            "running ls",
            "Cargo.toml\nsrc",
            "two entries",
            "thanks"
        ]
    );
    let tool = all.get(2).ok_or("the tool row")?;
    assert_eq!(tool.role, Role::Tool);
    assert_eq!(tool.tool_name.as_deref(), Some("Bash"));

    let detail = hub
        .get(ALEX, &format!("/api/sessions/{}", segment(ALEXS)?))
        .await?;
    assert_eq!(
        detail.body.get("messageCount").and_then(Value::as_u64),
        Some(5),
        "the catalog counts what the messages route returns"
    );
    let refused = hub
        .get(ALEX, &format!("/api/sessions/{TOMS}/messages"))
        .await?;
    assert_eq!(refused.status, 404, "{}", refused.body);
    let bad = hub
        .get(ALEX, &format!("/api/sessions/{ALEXS}/messages?cursor=x"))
        .await?;
    assert_eq!(bad.status, 400, "{}", bad.body);
    hub.stop().await
}

#[tokio::test]
async fn a_received_session_returns_its_original_text_and_says_whose_it_is() -> TestResult {
    let hub = Hub::start().await?;
    let captured = "claude-code:00000000-0000-4000-8000-00000000c004";
    hub.capture(
        PHIL,
        captured,
        &["a friend's question about the relay", "an answer"],
    )
    .await?;
    sqlx::query(
        "UPDATE rh_conversations SET session_key = $1,
                settings = '{\"origin\":{\"author\":\"friend\"}}'::jsonb
          WHERE session_key = $2",
    )
    .bind(RECEIVED)
    .bind(captured)
    .execute(&hub.db.pool())
    .await?;
    let received = page(&hub, PHIL, RECEIVED, "").await?;
    assert_eq!(received.total, 2);
    let first = received.items.first().ok_or("a message")?;
    assert_eq!(first.content, "a friend's question about the relay");
    let quoted = received
        .quoted_from
        .ok_or("a received session says it is quoted")?;
    assert_eq!(quoted.author.as_deref(), Some("friend"));
    hub.stop().await
}
