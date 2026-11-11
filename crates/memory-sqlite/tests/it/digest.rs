use crate::common;

use common::TestResult;
use common::batch;
use common::fresh_store;
use common::instant;
use common::message;
use common::object;
use kernel::memory::CaptureMessage;
use kernel::memory::SessionDeletion as _;
use kernel::memory::SessionDigest;
use kernel::owner::Owner;
use uuid::Uuid;
use wire::message::Role;

fn said(event_id: &str, at: &str) -> TestResult<CaptureMessage> {
    Ok(CaptureMessage {
        created_at: Some(instant(at)?),
        ..message(event_id, Role::User, &format!("said {event_id}"))
    })
}

#[tokio::test]
async fn digests_count_messages_and_dates_per_key() -> TestResult {
    let fresh = fresh_store()?;
    let store = &fresh.store;
    let phil = Owner::new("phil".to_owned())?;
    let now = instant("2026-11-02T18:00:00.000Z")?;
    let mut long = batch("claude-code:long", "rivet", "claude-code");
    long.title = Some("pairing".to_owned());
    long.messages = vec![
        said("evt-2", "2026-10-14T09:30:00.000Z")?,
        said("evt-1", "2026-10-14T09:00:00.000Z")?,
        said("evt-3", "2026-10-15T08:00:00.000Z")?,
    ];
    let long_id = store.capture(long, &phil, None, now).await?.conversation_id;
    let mut imported = batch(
        "import:[\"laptop\",\"00000000-0000-4000-8000-000000000009\"]",
        "codex",
        "codex",
    );
    imported.settings = Some(object(
        r#"{"origin":{"author":"ana","node":"laptop","conversation_id":"00000000-0000-4000-8000-000000000009","session_key":"codex:shared","agent":"codex","created_at":"2026-09-01T10:00:00Z","updated_at":"2026-09-01T10:00:00Z"}}"#,
    )?);
    imported.messages = vec![said("evt-i", "2026-09-01T10:00:00.000Z")?];
    let imported_key =
        wire::capture::owned_import_key("phil", &imported.session_key)?.ok_or("import key")?;
    let imported_id = store
        .capture(imported, &phil, None, now)
        .await?
        .conversation_id;
    let mut trashed = batch("claude-code:trashed", "rivet", "claude-code");
    trashed.messages = vec![said("evt-t", "2026-10-14T09:00:00.000Z")?];
    store.capture(trashed, &phil, None, now).await?;
    store
        .trash_session("claude-code:trashed", None, now)
        .await?;

    let digests = store
        .session_digests(
            &phil,
            vec![
                "claude-code:long".to_owned(),
                imported_key.clone(),
                "claude-code:trashed".to_owned(),
                "claude-code:never-captured".to_owned(),
            ],
        )
        .await?;

    assert_eq!(
        digests,
        [
            SessionDigest {
                key: "claude-code:long".to_owned(),
                conversation_id: Uuid::parse_str(&long_id)?,
                title: Some("pairing".to_owned()),
                agent: "rivet".to_owned(),
                first: instant("2026-10-14T09:00:00.000Z")?,
                last: instant("2026-10-15T08:00:00.000Z")?,
                messages: 3,
                imported_from: None,
            },
            SessionDigest {
                key: imported_key,
                conversation_id: Uuid::parse_str(&imported_id)?,
                title: None,
                agent: "codex".to_owned(),
                first: instant("2026-09-01T10:00:00.000Z")?,
                last: instant("2026-09-01T10:00:00.000Z")?,
                messages: 1,
                imported_from: Some("ana".to_owned()),
            },
        ]
    );
    Ok(())
}

#[tokio::test]
async fn digests_never_return_another_owner() -> TestResult {
    let fresh = fresh_store()?;
    let store = &fresh.store;
    let now = instant("2026-11-02T18:00:00.000Z")?;
    let mut phils = batch("claude-code:phil", "rivet", "claude-code");
    phils.messages = vec![said("evt-p", "2026-10-14T09:00:00.000Z")?];
    store
        .capture(phils, &Owner::new("phil".to_owned())?, None, now)
        .await?;
    let mut alexs = batch("claude-code:alex", "rivet", "claude-code");
    alexs.messages = vec![said("evt-a", "2026-10-14T09:00:00.000Z")?];
    store
        .capture(alexs, &Owner::new("alex".to_owned())?, None, now)
        .await?;

    let digests = store
        .session_digests(
            &Owner::new("phil".to_owned())?,
            vec!["claude-code:alex".to_owned(), "claude-code:phil".to_owned()],
        )
        .await?;

    let keys: Vec<&str> = digests.iter().map(|digest| digest.key.as_str()).collect();
    assert_eq!(keys, ["claude-code:phil"]);
    Ok(())
}
