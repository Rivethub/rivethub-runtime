use crate::common;

use common::TestResult;
use common::batch;
use common::execute;
use common::fresh_store;
use common::instant;
use common::message;
use common::object;
use common::table;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::SessionDeletion as _;
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
async fn export_takes_only_the_node_owners_sessions_and_credits_their_stored_owner_and_node()
-> TestResult {
    let source = fresh_store()?;
    let phil_owner = Owner::new("phil".to_owned())?;
    let alex = Owner::new("alex".to_owned())?;
    let now = instant("2026-11-02T18:00:00.000Z")?;
    let mut first = batch("claude-code:one", "rivet", "claude-code");
    first.title = Some("pairing".to_owned());
    first
        .messages
        .push(message("evt-1", Role::User, "ship the bundle"));
    first.messages.push(message(
        "evt-2",
        Role::Assistant,
        "the file names its author",
    ));
    source
        .store
        .capture(first, &phil_owner, Some("laptop"), now)
        .await?;
    let mut legacy = batch("codex:legacy", "rivet-gpt", "codex");
    legacy
        .messages
        .push(message("evt-3", Role::User, "captured before owners"));
    let legacy = source.store.capture(legacy, &phil_owner, None, now).await?;
    execute(
        &source.store,
        &format!(
            "UPDATE ros_conversations SET owner_user_id = NULL WHERE id = '{}'",
            legacy.conversation_id
        ),
    )
    .await?;
    let mut private = batch("claude-code:alex-private", "rivet", "claude-code");
    private
        .messages
        .push(message("evt-4", Role::User, "alex private note"));
    let private = source.store.capture(private, &alex, None, now).await?;

    let exported = source.store.bundled_sessions(phil(), None).await?;

    let text = serde_json::to_string(&exported)?;
    assert!(!text.contains("alex private note"));
    assert!(!text.contains("owner_user_id"));
    let mut origins: Vec<(&str, &str, &str)> = exported
        .iter()
        .map(|session| {
            (
                session.batch.session_key.as_str(),
                session.origin.author.as_str(),
                session.origin.node.as_str(),
            )
        })
        .collect();
    origins.sort_unstable();
    assert_eq!(
        origins,
        [
            ("claude-code:one", "phil", "laptop"),
            ("codex:legacy", "phil", "desk"),
        ]
    );
    let pairing = exported
        .iter()
        .find(|session| session.batch.session_key == "claude-code:one")
        .ok_or("pairing session")?;
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
    let named = source
        .store
        .bundled_sessions(phil(), Some(vec![private.conversation_id]))
        .await?;
    assert_eq!(named, Vec::new());
    Ok(())
}

#[tokio::test]
async fn a_bundled_batch_is_recorded_as_the_receivers_own() -> TestResult {
    let source = fresh_store()?;
    let phil_owner = Owner::new("phil".to_owned())?;
    let now = instant("2026-11-02T18:00:00.000Z")?;
    let mut first = batch("claude-code:one", "rivet", "claude-code");
    first
        .messages
        .push(message("evt-1", Role::User, "ship the bundle"));
    first.messages.push(message(
        "evt-2",
        Role::Assistant,
        "the file names its author",
    ));
    source.store.capture(first, &phil_owner, None, now).await?;
    let exported = source.store.bundled_sessions(phil(), None).await?;
    let pairing = exported.first().ok_or("pairing session")?;

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

#[tokio::test]
async fn a_trashed_session_is_exported_only_when_named() -> TestResult {
    let source = fresh_store()?;
    let phil_owner = Owner::new("phil".to_owned())?;
    let now = instant("2026-11-02T18:00:00.000Z")?;
    let mut kept = batch("claude-code:kept", "rivet", "claude-code");
    kept.messages.push(message("evt-k", Role::User, "kept"));
    let kept = source.store.capture(kept, &phil_owner, None, now).await?;
    let mut trashed = batch("claude-code:trashed", "rivet", "claude-code");
    trashed
        .messages
        .push(message("evt-t", Role::User, "trashed"));
    let trashed = source
        .store
        .capture(trashed, &phil_owner, None, now)
        .await?;
    source
        .store
        .trash_session("claude-code:trashed", None, now)
        .await?;

    let everything = source.store.bundled_sessions(phil(), None).await?;
    let named = source
        .store
        .bundled_sessions(phil(), Some(vec![trashed.conversation_id.clone()]))
        .await?;

    let ids = |sessions: &[wire::capture::BundledSession]| -> Vec<String> {
        sessions
            .iter()
            .map(|session| session.origin.conversation_id.clone())
            .collect()
    };
    assert_eq!(ids(&everything), [kept.conversation_id]);
    assert_eq!(ids(&named), [trashed.conversation_id]);
    Ok(())
}

#[tokio::test]
async fn export_orders_sessions_by_creation_then_id_and_messages_by_time_then_row() -> TestResult {
    let source = fresh_store()?;
    execute(
        &source.store,
        "INSERT INTO ros_conversations (id, session_key, agent, settings, created_at, updated_at) VALUES
           ('b-first', 'claude-code:first', 'rivet', '{}', '2026-10-14T09:00:00.000Z', '2026-10-14T10:00:00.000Z'),
           ('a-second', 'claude-code:second', 'rivet', '{}', '2026-10-15T09:00:00.000Z', '2026-10-15T10:00:00.000Z'),
           ('c-second', 'claude-code:third', 'rivet', '{}', '2026-10-15T09:00:00.000Z', '2026-10-15T10:00:00.000Z');
         INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, metadata, created_at) VALUES
           ('m3', 'b-first', 'rivet', 'claude-code', 'user', 'later', '{}', '2026-10-14T09:05:00.000Z'),
           ('m2', 'b-first', 'rivet', 'claude-code', 'user', 'tied one', '{}', '2026-10-14T09:00:00.000Z'),
           ('m1', 'b-first', 'rivet', 'claude-code', 'user', 'tied two', '{}', '2026-10-14T09:00:00.000Z'),
           ('m4', 'c-second', 'rivet', 'claude-code', 'user', 'third', '{}', '2026-10-15T09:00:00.000Z'),
           ('m5', 'a-second', 'rivet', 'claude-code', 'user', 'second', '{}', '2026-10-15T09:00:00.000Z');",
    )
    .await?;

    let exported = source.store.bundled_sessions(phil(), None).await?;

    let order: Vec<(&str, Vec<&str>)> = exported
        .iter()
        .map(|session| {
            (
                session.origin.conversation_id.as_str(),
                session
                    .batch
                    .messages
                    .iter()
                    .map(|message| message.content.as_str())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            ("b-first", vec!["tied one", "tied two", "later"]),
            ("a-second", vec!["second"]),
            ("c-second", vec!["third"]),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn an_imported_session_keeps_its_origin_and_dates() -> TestResult {
    let source = fresh_store()?;
    let phil_owner = Owner::new("phil".to_owned())?;
    let mut shared = batch("claude-code:shared", "rivet", "claude-code");
    shared
        .messages
        .push(message("evt-1", Role::User, "move the bundle reader"));
    source
        .store
        .capture(
            shared,
            &phil_owner,
            None,
            instant("2026-10-14T09:00:00.000Z")?,
        )
        .await?;
    let exported = source.store.bundled_sessions(phil(), None).await?;
    let session = exported.first().ok_or("exported session")?;
    let received: CaptureBatchRequest =
        serde_json::from_str(&serde_json::to_string(&session.import_batch()?)?)?;
    let destination = fresh_store()?;
    let bob = Owner::new("bob".to_owned())?;
    let later = instant("2026-11-08T12:00:00.000Z")?;

    let first = destination
        .store
        .capture(received.clone(), &bob, None, later)
        .await?;
    let again = destination
        .store
        .capture(received, &bob, None, later)
        .await?;

    assert_eq!((first.inserted, again.inserted), (1, 0));
    let conversations = table(&destination.store, "ros_conversations").await?;
    let conversation = conversations.first().ok_or("imported conversation")?;
    let column = |name: &str| conversation.get(name).and_then(testkit::Json::as_str);
    assert_eq!(column("created_at"), Some("2026-10-14T09:00:00.000Z"));
    assert_eq!(column("updated_at"), Some("2026-10-14T09:00:00.000Z"));
    assert_eq!(column("owner_user_id"), Some("bob"));
    let messages = table(&destination.store, "ros_messages").await?;
    let id = messages
        .first()
        .and_then(|row| row.get("id"))
        .and_then(testkit::Json::as_str)
        .ok_or("imported message")?;
    let entry = destination.store.get_full(id).await?.ok_or("full entry")?;
    let metadata = entry.metadata.ok_or("metadata")?;
    let origin: wire::capture::SessionOrigin =
        serde_json::from_value(metadata.get("origin").cloned().ok_or("origin")?)?;
    assert_eq!(origin, session.origin);
    Ok(())
}
