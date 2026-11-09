use crate::common;

use common::TestResult;
use common::batch;
use common::fresh_store;
use common::instant;
use common::message;
use common::object;
use common::table;
use kernel::memory::CaptureBatchRequest;
use kernel::owner::Owner;
use wire::capture::BundleAuthor;
use wire::message::Role;

fn phil() -> BundleAuthor {
    BundleAuthor {
        user: "phil".to_owned(),
        node: "desk".to_owned(),
    }
}

#[tokio::test]
async fn a_bundle_credits_its_author_and_the_receiver_records_its_own_owner() -> TestResult {
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

    let exported = source.store.bundled_sessions(phil(), None).await?;
    let text = serde_json::to_string(&exported)?;
    assert!(!text.contains("alice"));
    assert!(!text.contains("owner_user_id"));
    assert_eq!(exported.len(), 2);
    let pairing = exported
        .iter()
        .find(|session| session.batch.session_key == "claude-code:one")
        .ok_or("pairing session")?;
    assert_eq!(pairing.origin.author, "phil");
    assert_eq!(pairing.origin.node, "desk");
    assert_eq!(pairing.batch.title.as_deref(), Some("pairing"));
    assert_eq!(
        pairing
            .batch
            .messages
            .iter()
            .map(|message| message.event_id.as_str())
            .collect::<Vec<_>>(),
        ["evt-1", "evt-2"]
    );

    let received: CaptureBatchRequest =
        serde_json::from_value(serde_json::to_value(&pairing.batch)?)?;
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
    Ok(())
}

#[tokio::test]
async fn export_by_id_keeps_settings() -> TestResult {
    let source = fresh_store()?;
    let phil_owner = Owner::new("phil".to_owned())?;
    let now = instant("2026-11-02T18:00:00.000Z")?;
    let mut wanted = batch("claude-code:wanted", "rivet", "claude-code");
    wanted.settings = Some(object(r#"{"model":"opus","cwd":"/work/rivethub"}"#)?);
    wanted
        .messages
        .push(message("evt-1", Role::User, "export me"));
    let kept = source.store.capture(wanted, &phil_owner, None, now).await?;
    let mut other = batch("claude-code:other", "rivet", "claude-code");
    other
        .messages
        .push(message("evt-2", Role::User, "leave me"));
    source.store.capture(other, &phil_owner, None, now).await?;

    let exported = source
        .store
        .bundled_sessions(phil(), Some(vec![kept.conversation_id.clone()]))
        .await?;

    assert_eq!(exported.len(), 1);
    let session = exported.first().ok_or("exported session")?;
    assert_eq!(session.origin.conversation_id, kept.conversation_id);
    assert_eq!(session.batch.session_key, "claude-code:wanted");
    assert_eq!(
        session.batch.settings,
        Some(object(r#"{"model":"opus","cwd":"/work/rivethub"}"#)?)
    );
    let none = source
        .store
        .bundled_sessions(phil(), Some(vec!["no-such-conversation".to_owned()]))
        .await?;
    assert_eq!(none, Vec::new());
    Ok(())
}
