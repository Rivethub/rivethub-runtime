use crate::common;

use common::TestResult;
use common::batch;
use common::fresh_store;
use common::instant;
use common::message;
use common::table;
use kernel::memory::CaptureBatchRequest;
use kernel::owner::Owner;
use wire::message::Role;

#[tokio::test]
async fn a_bundle_names_no_owner_and_the_receiver_records_its_own() -> TestResult {
    let source = fresh_store()?;
    let alice = Owner::new("alice".to_owned())?;
    let now = instant("2026-11-02T18:00:00.000Z")?;
    let mut first = batch("claude-code:one", "rivet", "claude-code");
    first.title = Some("pairing".to_owned());
    first
        .messages
        .push(message("evt-1", Role::User, "ship the bundle"));
    first
        .messages
        .push(message("evt-2", Role::Assistant, "the file names no owner"));
    source.store.capture(first, &alice, None, now).await?;
    let mut second = batch("codex:two", "rivet-gpt", "codex");
    second
        .messages
        .push(message("evt-3", Role::User, "ingest it there"));
    source.store.capture(second, &alice, None, now).await?;

    let exported = source.store.session_bundle().await?;
    let text = serde_json::to_string(&exported)?;
    assert!(!text.contains("alice"));
    assert!(!text.contains("owner_user_id"));
    assert_eq!(exported.sessions.len(), 2);
    let pairing = exported
        .sessions
        .iter()
        .find(|session| session.session_key == "claude-code:one")
        .ok_or("pairing session")?;
    assert_eq!(pairing.title.as_deref(), Some("pairing"));
    assert_eq!(
        pairing
            .messages
            .iter()
            .map(|message| message.event_id.as_str())
            .collect::<Vec<_>>(),
        ["evt-1", "evt-2"]
    );
    let first_message = pairing.messages.first().ok_or("pairing message")?;
    assert_eq!(first_message.content, "ship the bundle");

    let received: CaptureBatchRequest = serde_json::from_value(serde_json::to_value(pairing)?)?;
    let destination = fresh_store()?;
    let bob = Owner::new("bob".to_owned())?;
    destination.store.capture(received, &bob, None, now).await?;
    let rows = table(&destination.store, "ros_messages").await?;
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(
            row.get("owner_user_id").and_then(testkit::Json::as_str),
            Some("bob")
        );
    }
    let again = destination.store.session_bundle().await?;
    let again_pairing = again
        .sessions
        .iter()
        .find(|session| session.session_key == "claude-code:one")
        .ok_or("ingested pairing")?;
    assert_eq!(
        again_pairing
            .messages
            .iter()
            .map(|message| message.event_id.as_str())
            .collect::<Vec<_>>(),
        ["evt-1", "evt-2"]
    );
    Ok(())
}
