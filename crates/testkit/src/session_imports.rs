use kernel::memory::CaptureBatchRequest;
use kernel::memory::ImportOrigin;
use kernel::memory::ImportReceipt;
use kernel::memory::MemoryState;
use kernel::memory::MemoryStore;
use kernel::memory::MemoryStoreError;
use kernel::memory::SessionCatalog;
use kernel::memory::SessionDeletion;
use kernel::memory::SessionQuery;
use kernel::memory::SessionRecord;
use kernel::memory::SessionView;
use kernel::memory::share::receive::Verification;
use wire::capture::BundledSession;
use wire::capture::SessionOrigin;
use wire::harness::HarnessId;
use wire::message::Role;
use wire::session_id::SessionId;
use wire::session_id::SessionKey;

use crate::session_catalog::seed;
use crate::topics::Outcome;
use crate::topics::at;
use crate::topics::same;

const UNKNOWN: &str = "unknown";

pub(crate) async fn record_of(
    catalog: &dyn SessionCatalog,
    session: &SessionId,
) -> Result<Option<SessionRecord>, MemoryStoreError> {
    Ok(catalog
        .get(&SessionKey::from(session))
        .await?
        .and_then(SessionRecord::narrow))
}

fn message(
    event_id: &str,
    role: Role,
    content: &str,
    minute: u32,
) -> wire::capture::CaptureMessage {
    wire::capture::CaptureMessage {
        event_id: event_id.to_owned(),
        role,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(at(minute)),
    }
}

fn bundle(author: &str, node: &str, conversation: &str, minute: u32) -> BundledSession {
    BundledSession {
        origin: SessionOrigin {
            author: author.to_owned(),
            node: node.to_owned(),
            conversation_id: conversation.to_owned(),
            session_key: "claude-code:shared".to_owned(),
            agent: "rivet".to_owned(),
            created_at: Some(at(minute)),
            updated_at: Some(at(minute + 5)),
            identity: None,
        },
        batch: wire::capture::CaptureBatchRequest {
            session_key: "claude-code:shared".to_owned(),
            agent: "rivet".to_owned(),
            channel: Some("claude-code".to_owned()),
            title: Some(format!("{author}'s shared session")),
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![
                message(
                    &format!("{conversation}-1"),
                    Role::User,
                    "move the reader",
                    minute,
                ),
                message(
                    &format!("{conversation}-2"),
                    Role::Assistant,
                    "the reader moved",
                    minute + 5,
                ),
            ],
        },
    }
}

async fn imported(
    memory: &dyn MemoryStore,
    session: &BundledSession,
) -> Result<SessionKey, Box<dyn std::error::Error>> {
    stored(memory, session.import_batch()?).await
}

async fn received(
    memory: &dyn MemoryStore,
    session: &BundledSession,
    receipt: &wire::capture::Receipt,
) -> Result<SessionKey, Box<dyn std::error::Error>> {
    stored(memory, session.received_batch(receipt)?).await
}

async fn stored(
    memory: &dyn MemoryStore,
    batch: wire::capture::CaptureBatchRequest,
) -> Result<SessionKey, Box<dyn std::error::Error>> {
    let owner = crate::owner()?;
    let session_key = batch.session_key.clone();
    let posted: CaptureBatchRequest = serde_json::from_value(serde_json::to_value(batch)?)?;
    memory.capture(posted, &owner, None).await?;
    let key =
        wire::capture::owned_import_key(owner.as_str(), &session_key)?.ok_or("an import key")?;
    Ok(key.parse()?)
}

async fn views(
    catalog: &dyn SessionCatalog,
    key: &SessionKey,
) -> Result<Vec<SessionView>, MemoryStoreError> {
    let mut found = Vec::new();
    for view in [
        SessionView::All,
        SessionView::Unfiled,
        SessionView::Kept,
        SessionView::Trash,
    ] {
        let page = catalog.list(&SessionQuery::new(view)).await?;
        if page.items.iter().any(|item| &item.id == key) {
            found.push(view);
        }
    }
    Ok(found)
}

pub async fn an_import_lists_with_its_origin(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
) -> Outcome {
    let seeded = seed(memory).await?;
    let known = bundle("alex", "desk", "7b0e7d1c-0000-4000-8000-000000000001", 50);
    let anonymous = bundle(UNKNOWN, UNKNOWN, UNKNOWN, 70);
    let receipt = wire::capture::Receipt {
        bundle_id: "drop-1".to_owned(),
        received_at: at(90),
        verification: wire::capture::ReceiptVerification::UnknownKey,
        key_fingerprint: Some("ed25519:abc".to_owned()),
        checks: None,
    };
    let key = received(memory, &known, &receipt).await?;
    let hidden = imported(memory, &anonymous).await?;

    let all = catalog.list(&SessionQuery::new(SessionView::All)).await?;
    let item = all
        .items
        .iter()
        .find(|item| item.id == key)
        .ok_or("the import is listed")?;
    same(
        "the origin names the author and the author's dates",
        item.origin.clone(),
        Some(ImportOrigin {
            author: Some("alex".to_owned()),
            created_at: known.origin.created_at,
            updated_at: known.origin.updated_at,
            receipt: Some(ImportReceipt {
                bundle_id: "drop-1".to_owned(),
                verification: Verification::UnknownKey,
            }),
        }),
    )?;
    same(
        "the import starts when the author started",
        item.started_at,
        at(50),
    )?;
    same("two messages", item.message_count, 2)?;
    same("it is not trashed", item.trashed, false)?;
    same(
        "its memory state is read",
        item.memory != MemoryState::Flagged,
        true,
    )?;
    let unnamed = all
        .items
        .iter()
        .find(|item| item.id == hidden)
        .ok_or("the import with no known origin is listed")?;
    same(
        "an unknown author is no author",
        unnamed.origin.as_ref().map(|origin| origin.author.clone()),
        Some(None),
    )?;
    same(
        "a native session carries no origin",
        all.items
            .iter()
            .filter(|item| item.id.session() == Some(&seeded.alpha))
            .map(|item| item.origin.clone())
            .collect::<Vec<_>>(),
        vec![None],
    )?;
    same(
        "an import lists in no view but its own",
        views(catalog, &key).await?,
        vec![SessionView::All, SessionView::Unfiled],
    )?;
    Ok(())
}

pub async fn a_harness_filter_and_the_session_lookup_leave_imports_to_their_own_key(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
) -> Outcome {
    let seeded = seed(memory).await?;
    let key = imported(memory, &bundle("alex", "desk", "conv-a", 50)).await?;

    let mut claude = SessionQuery::new(SessionView::All);
    claude.harness = Some(HarnessId::ClaudeCode);
    let filtered = catalog.list(&claude).await?;
    same(
        "a harness filter leaves the import out",
        filtered.items.iter().any(|item| item.id == key),
        false,
    )?;
    same(
        "and keeps the harness sessions",
        filtered.items.is_empty(),
        false,
    )?;
    let asked = [SessionKey::from(&seeded.alpha), key.clone()];
    let entries = catalog.entries(&asked).await?;
    same("entries answers both kinds of key", entries.len(), 2)?;
    same(
        "get answers an import",
        catalog.get(&key).await?.map(|record| record.id),
        Some(key),
    )?;
    let natives = catalog.records(std::slice::from_ref(&seeded.alpha)).await?;
    same(
        "records answers a native session without an origin",
        natives
            .iter()
            .map(|record| record.origin.clone())
            .collect::<Vec<_>>(),
        vec![None],
    )?;
    Ok(())
}

pub async fn an_import_moves_between_the_views_and_deletes(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
    marks: &dyn SessionDeletion,
) -> Outcome {
    let key = imported(memory, &bundle("alex", "desk", "conv-b", 50)).await?;
    let text = key.as_str();
    let now = at(90);
    same(
        "imported",
        views(catalog, &key).await?,
        vec![SessionView::All, SessionView::Unfiled],
    )?;

    marks.keep_session(text, Some("owner"), now).await?;
    same(
        "kept",
        views(catalog, &key).await?,
        vec![SessionView::All, SessionView::Unfiled, SessionView::Kept],
    )?;
    marks.unkeep_session(text).await?;

    marks.trash_session(text, Some("owner"), now).await?;
    marks.trash_session(text, Some("owner"), now).await?;
    same(
        "trashed",
        views(catalog, &key).await?,
        vec![SessionView::Trash],
    )?;
    same(
        "the record says so",
        catalog.get(&key).await?.map(|record| record.trashed),
        Some(true),
    )?;

    marks.restore_session(text).await?;
    same(
        "restored",
        views(catalog, &key).await?,
        vec![SessionView::All, SessionView::Unfiled],
    )?;

    marks.trash_session(text, Some("owner"), now).await?;
    marks.delete_session(text, Some("owner"), now).await?;
    same("deleted", views(catalog, &key).await?, Vec::new())?;
    same("and gone from a lookup", catalog.get(&key).await?, None)?;
    Ok(())
}

pub async fn paging_walks_across_an_import(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
) -> Outcome {
    let seeded = seed(memory).await?;
    let key = imported(memory, &bundle("alex", "desk", "conv-c", 25)).await?;
    let mut query = SessionQuery::new(SessionView::All);
    query.limit = 1;
    let mut walked = Vec::new();
    for _ in 0..8 {
        let page = catalog.list(&query).await?;
        walked.extend(page.items.into_iter().map(|item| item.id.to_string()));
        match page.next {
            Some(next) => query.after = Some(next),
            None => break,
        }
    }
    same(
        "one-row pages visit each session once, the import between its neighbours",
        walked,
        vec![
            seeded.delta.to_string(),
            seeded.gamma.to_string(),
            key.to_string(),
            seeded.beta.to_string(),
            seeded.alpha.to_string(),
        ],
    )?;
    Ok(())
}

pub async fn an_import_is_a_finished_record_ended_when_its_origin_ended(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
) -> Outcome {
    let dated = bundle("alex", "desk", "conv-d", 50);
    let mut undated = bundle("alex", "desk", "conv-e", 70);
    undated.origin.updated_at = None;
    let dated_key = imported(memory, &dated).await?;
    let undated_key = imported(memory, &undated).await?;

    let row = |key: &SessionKey| {
        let key = key.clone();
        async move { catalog.get(&key).await }
    };
    let first = row(&dated_key).await?.ok_or("the import is listed")?;
    same(
        "an import is not capturing",
        first.memory != MemoryState::Capturing,
        true,
    )?;
    same(
        "it ended when the author last updated it",
        first.ended_at,
        dated.origin.updated_at,
    )?;
    let open = row(&undated_key).await?.ok_or("the undated import")?;
    same(
        "an undated import is not capturing",
        open.memory != MemoryState::Capturing,
        true,
    )?;
    same(
        "an undated import ended at its last message",
        open.ended_at,
        Some(at(75)),
    )?;

    imported(memory, &dated).await?;
    let again = row(&dated_key).await?.ok_or("the import is still listed")?;
    same(
        "importing the bundle again keeps it ended",
        again.ended_at,
        dated.origin.updated_at,
    )?;
    same(
        "and still finished",
        again.memory != MemoryState::Capturing,
        true,
    )?;
    Ok(())
}
