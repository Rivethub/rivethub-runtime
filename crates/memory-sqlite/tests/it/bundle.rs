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
use kernel::memory::MemoryStore as _;
use kernel::memory::SessionCatalog as _;
use kernel::memory::SessionDeletion as _;
use kernel::memory::SessionQuery;
use kernel::memory::SessionView;
use kernel::memory::SummaryStore as _;
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
    let receiver_key =
        wire::capture::owned_import_key("bob", &session.import_batch()?.session_key)?
            .ok_or("an import key")?;
    assert_eq!(column("session_key"), Some(receiver_key.as_str()));
    let origins = destination
        .store
        .lookup_origins(vec![receiver_key.clone()])
        .await?;
    let provenance = origins.get(&receiver_key).ok_or("origin")?;
    assert_eq!(
        provenance.author.as_deref(),
        Some(session.origin.author.as_str())
    );
    assert_eq!(provenance.origin_date, session.origin.created_at);
    let rows = destination
        .store
        .browse(kernel::memory::BrowseFilter {
            conversation_id: None,
            agent: None,
            role: None,
            tool_name: None,
            tag: None,
            window: None,
            since: None,
            before: None,
            include_tools: false,
            order: kernel::memory::MemoryBrowseOrder::Desc,
            limit: kernel::memory::Limit::clamped(None, 50, 1..=200),
        })
        .await?;
    let browsed = rows
        .iter()
        .filter_map(|row| row.provenance.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(browsed.len(), rows.len());
    assert!(
        browsed
            .iter()
            .all(|row| row.author.as_deref() == Some("phil"))
    );
    Ok(())
}

#[tokio::test]
async fn messages_sharing_a_millisecond_export_in_capture_order() -> TestResult {
    let store = fresh_store()?;
    let alex = Owner::new("alex".to_owned())?;
    let tied = instant("2026-10-14T09:00:00.123Z")?;
    let steps: Vec<String> = (1..=8).map(|step| format!("step-{step}")).collect();
    let mut tied_batch = batch("claude-code:tied", "rivet", "claude-code");
    for step in &steps {
        let mut said = message(step, Role::User, step);
        said.created_at = Some(tied);
        tied_batch.messages.push(said);
    }
    store
        .store
        .capture(
            tied_batch,
            &alex,
            None,
            instant("2026-11-08T12:00:00.000Z")?,
        )
        .await?;

    let exported = store
        .store
        .bundled_sessions(
            BundleAuthor {
                user: "alex".to_owned(),
                node: "team".to_owned(),
            },
            None,
        )
        .await?;

    let session = exported.first().ok_or("exported session")?;
    let order: Vec<&str> = session
        .batch
        .messages
        .iter()
        .map(|message| message.event_id.as_str())
        .collect();
    assert_eq!(order, steps);
    Ok(())
}

#[tokio::test]
async fn messages_sharing_a_time_keep_capture_order_in_summaries_context_and_titles() -> TestResult
{
    let store = fresh_store()?;
    let alex = Owner::new("alex".to_owned())?;
    let tied = instant("2026-10-14T09:00:00.123Z")?;
    let steps: Vec<String> = (1..=8).map(|step| format!("step-{step}")).collect();
    let mut tied_batch = batch("claude-code:tied", "rivet", "claude-code");
    for step in &steps {
        let mut said = message(step, Role::User, step);
        said.created_at = Some(tied);
        tied_batch.messages.push(said);
    }
    let captured = store
        .store
        .capture(
            tied_batch,
            &alex,
            None,
            instant("2026-11-08T12:00:00.000Z")?,
        )
        .await?;

    let window = store
        .store
        .conversation(uuid::Uuid::parse_str(&captured.conversation_id)?)
        .await?
        .ok_or("summary window")?;
    let summarized: Vec<&str> = window
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect();
    assert_eq!(summarized, steps);

    let context = store
        .store
        .context("claude-code:tied", instant("2026-11-08T12:00:00.000Z")?)
        .await?;
    let (older, newest) = steps.split_at(3);
    let recent = newest.iter().map(|step| format!("[user] {step}"));
    let relevant = older.iter().map(|step| format!("[rivet/user] {step}"));
    assert!(
        context.contains(&recent.collect::<Vec<_>>().join("\n")),
        "{context}"
    );
    assert!(
        context.contains(&relevant.collect::<Vec<_>>().join("\n")),
        "{context}"
    );

    let page = store
        .store
        .list(&SessionQuery::new(SessionView::All))
        .await?;
    let row = page.items.first().ok_or("catalog row")?;
    assert_eq!(row.title, "step-1");
    Ok(())
}

#[derive(serde::Deserialize)]
struct Settings {
    pinned: Option<bool>,
    model: Option<String>,
    origin: Option<wire::capture::SessionOrigin>,
}

fn imported(
    origin_updated_at: &str,
    model: &str,
) -> Result<CaptureBatchRequest, Box<dyn std::error::Error>> {
    let mut shared = batch("claude-code:shared", "rivet", "claude-code");
    shared.settings = Some(serde_json::from_str(&format!(r#"{{"model":"{model}"}}"#))?);
    shared
        .messages
        .push(message("evt-1", Role::User, "move the bundle reader"));
    let session = wire::capture::BundledSession {
        origin: wire::capture::SessionOrigin {
            author: "phil".to_owned(),
            node: "desk".to_owned(),
            conversation_id: "7b0e7d1c-0000-4000-8000-000000000001".to_owned(),
            session_key: "claude-code:shared".to_owned(),
            agent: "rivet".to_owned(),
            created_at: Some(instant("2026-10-14T09:00:00.000Z")?),
            updated_at: Some(instant(origin_updated_at)?),
            identity: None,
        },
        batch: serde_json::from_value(serde_json::to_value(shared)?)?,
    };
    Ok(serde_json::from_str(&serde_json::to_string(
        &session.import_batch()?,
    )?)?)
}

#[tokio::test]
async fn a_reimport_merges_settings_and_keeps_the_newest_origin() -> TestResult {
    let destination = fresh_store()?;
    let bob = Owner::new("bob".to_owned())?;
    let later = instant("2026-11-08T12:00:00.000Z")?;
    let first = destination
        .store
        .capture(
            imported("2026-10-15T00:00:00.000Z", "opus")?,
            &bob,
            None,
            later,
        )
        .await?;
    execute(
        &destination.store,
        &format!(
            "UPDATE ros_conversations SET settings = json_set(settings, '$.pinned', json('true')) \
             WHERE id = '{}'",
            first.conversation_id
        ),
    )
    .await?;
    let settings = || async {
        let rows = table(&destination.store, "ros_conversations").await?;
        let row = rows.first().ok_or("conversation")?;
        let text = row
            .get("settings")
            .and_then(testkit::Json::as_str)
            .ok_or("settings")?;
        Ok::<Settings, Box<dyn std::error::Error>>(serde_json::from_str(text)?)
    };

    destination
        .store
        .capture(
            imported("2026-10-14T00:00:00.000Z", "sonnet")?,
            &bob,
            None,
            later,
        )
        .await?;
    let after_older = settings().await?;
    destination
        .store
        .capture(
            imported("2026-10-20T00:00:00.000Z", "haiku")?,
            &bob,
            None,
            later,
        )
        .await?;
    let after_newer = settings().await?;

    let updated = |settings: &Settings| {
        settings
            .origin
            .as_ref()
            .and_then(|origin| origin.updated_at)
    };
    assert_eq!(after_older.pinned, Some(true));
    assert_eq!(after_older.model.as_deref(), Some("opus"));
    assert_eq!(
        updated(&after_older),
        Some(instant("2026-10-15T00:00:00.000Z")?)
    );
    assert_eq!(after_newer.pinned, Some(true));
    assert_eq!(after_newer.model.as_deref(), Some("haiku"));
    assert_eq!(
        updated(&after_newer),
        Some(instant("2026-10-20T00:00:00.000Z")?)
    );
    Ok(())
}

#[tokio::test]
async fn an_older_bundle_keeps_a_newer_title() -> TestResult {
    let destination = fresh_store()?;
    let bob = Owner::new("bob".to_owned())?;
    let later = instant("2026-11-08T12:00:00.000Z")?;
    let titled = |updated: &str, title: &str| {
        let mut batch = imported(updated, "opus")?;
        batch.title = Some(title.to_owned());
        Ok::<_, Box<dyn std::error::Error>>(batch)
    };
    let title = || async {
        let rows = table(&destination.store, "ros_conversations").await?;
        let row = rows.first().ok_or("conversation")?;
        Ok::<_, Box<dyn std::error::Error>>(
            row.get("title")
                .and_then(testkit::Json::as_str)
                .map(str::to_owned),
        )
    };
    let first = destination
        .store
        .capture(
            titled("2026-10-20T00:00:00.000Z", "Wombat plan final")?,
            &bob,
            None,
            later,
        )
        .await?;
    let older = titled("2026-10-02T00:00:00.000Z", "Wombat draft")?;
    destination
        .store
        .capture(older.clone(), &bob, None, later)
        .await?;
    let after_older = title().await?;
    execute(
        &destination.store,
        &format!(
            "UPDATE ros_conversations SET title = 'Receiver title' WHERE id = '{}'",
            first.conversation_id
        ),
    )
    .await?;

    destination.store.capture(older, &bob, None, later).await?;

    assert_eq!(after_older.as_deref(), Some("Wombat plan final"));
    assert_eq!(title().await?.as_deref(), Some("Receiver title"));
    Ok(())
}

#[tokio::test]
async fn a_trashed_import_stays_trashed_on_reimport_and_exports_only_when_named() -> TestResult {
    let store = fresh_store()?;
    let alex = Owner::new("alex".to_owned())?;
    let now = instant("2026-11-08T12:00:00.000Z")?;
    let posted = imported("2026-10-14T11:30:00.000Z", "opus")?;
    let key = wire::capture::owned_import_key("alex", &posted.session_key)?.ok_or("import key")?;
    let first = store
        .store
        .capture(posted.clone(), &alex, None, now)
        .await?;
    store.store.trash_session(&key, Some("alex"), now).await?;

    let again = store.store.capture(posted, &alex, None, now).await?;

    assert_eq!(again.conversation_id, first.conversation_id);
    assert_eq!((again.inserted, again.skipped), (0, 1));
    assert!(
        store
            .store
            .trashed(std::slice::from_ref(&key))
            .await?
            .contains_key(&key)
    );
    let team = BundleAuthor {
        user: "alex".to_owned(),
        node: "team".to_owned(),
    };
    let everything = store.store.bundled_sessions(team.clone(), None).await?;
    assert_eq!(everything.len(), 0);
    let named = store
        .store
        .bundled_sessions(team, Some(vec![first.conversation_id.clone()]))
        .await?;
    let forwarded = named
        .into_iter()
        .next()
        .ok_or("the named trashed import")?
        .forwarding_stored_origin();
    assert_eq!(forwarded.origin.author, "phil");
    assert_eq!(forwarded.batch.session_key, "claude-code:shared");
    Ok(())
}
