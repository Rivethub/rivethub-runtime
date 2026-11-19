use jiff::SignedDuration;
use jiff::Timestamp;
use kernel::memory::MessagesQuery;
use kernel::memory::settlement::SettleStatus;
use kernel::memory::{
    CaptureBatchRequest, CaptureMessage, MemoryStore, SessionCatalog, SummaryStore,
};
use kernel::owner::Owner;
use serde_json::json;
use uuid::Uuid;
use wire::{JsonObject, message::Role};

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

pub fn batch(key: &str, messages: Vec<CaptureMessage>) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: key.to_owned(),
        channel: Some("test".to_owned()),
        agent: "settlement-test".to_owned(),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages,
    }
}

pub fn message(id: &str, at: &str) -> Result<CaptureMessage, jiff::Error> {
    Ok(CaptureMessage {
        event_id: id.to_owned(),
        role: Role::User,
        content: format!("Inspect the invented sample {id}."),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(at.parse()?),
    })
}

pub fn obj(value: serde_json::Value) -> Result<JsonObject, serde_json::Error> {
    serde_json::from_value(value)
}

pub fn observed(
    request: CaptureBatchRequest,
    event: &str,
    at: &str,
    end: bool,
) -> Result<CaptureBatchRequest, serde_json::Error> {
    Ok(CaptureBatchRequest {
        settings: Some(obj(json!({
            "source": "claude-code-hook",
            "last_event": event,
            "last_ingest_at": at,
            "last_event_id": format!("{event}/{at}"),
        }))?),
        finalize: end.then_some(true),
        ..request
    })
}

const KEY: &str = "claude-code:settlement-fixture";

pub async fn revision_lifecycle<S: MemoryStore + SummaryStore>(
    store: &S,
    owner: &Owner,
) -> TestResult {
    let first = batch(KEY, vec![message("one", "2026-01-03T01:00:00Z")?]);
    let id: Uuid = store
        .capture(first.clone(), owner, None)
        .await?
        .conversation_id
        .parse()?;
    let initial = store
        .summary_state(id)
        .await?
        .ok_or("first capture state")?;
    assert_eq!(initial.input_revision, 1);
    assert_eq!(initial.unsettled_since, Some(initial.last_input_at));
    assert!(!initial.historical_only);
    assert!(!store.summary_eligible(id, 1).await?);

    let marked = store
        .mark_summary_settled(KEY, owner)
        .await?
        .ok_or("manual mark")?;
    assert_eq!(marked.status, SettleStatus::Settled);
    let repeated = store
        .mark_summary_settled(&id.to_string(), owner)
        .await?
        .ok_or("repeat mark")?;
    assert_eq!(repeated.status, SettleStatus::AlreadySettled);
    assert_eq!(repeated.settled_at, marked.settled_at);
    assert!(store.summary_eligible(id, 1).await?);
    assert!(!store.summary_eligible(id, 0).await?);

    store.capture(first, owner, None).await?;
    let duplicate = store.summary_state(id).await?.ok_or("duplicate state")?;
    assert_eq!(duplicate.input_revision, 1);
    assert_eq!(duplicate.settled_at, marked.settled_at);
    assert_eq!(duplicate.last_input_at, initial.last_input_at);

    observe_reopened_revision(store, owner, id, initial.last_input_at).await
}

async fn observe_reopened_revision(
    store: &(impl MemoryStore + SummaryStore),
    owner: &Owner,
    id: Uuid,
    last_input_at: Timestamp,
) -> TestResult {
    let two = message("two", "2026-01-03T01:01:00Z")?;
    let reopen = observed(
        batch(KEY, vec![two]),
        "Stop",
        "2026-01-03T02:08:00.123456+01:00",
        false,
    )?;
    store.capture(reopen, owner, None).await?;
    let active = store.summary_state(id).await?.ok_or("reopened state")?;
    assert_eq!(active.input_revision, 2);
    assert_eq!(active.unsettled_since, Some(active.last_input_at));
    assert!(active.last_input_at >= last_input_at);
    assert!(!store.summary_eligible(id, 1).await?);
    assert!(!store.summary_eligible(id, 2).await?);

    let superseded = observed(
        batch(KEY, vec![]),
        "SessionEnd",
        "2026-01-03T01:08:00.123455Z",
        true,
    )?;
    store.capture(superseded, owner, None).await?;
    assert_eq!(store.summary_state(id).await?, Some(active.clone()));

    let end = observed(
        batch(KEY, vec![]),
        "SessionEnd",
        "2026-01-03T01:08:00.123457Z",
        true,
    )?;
    store.capture(end.clone(), owner, None).await?;
    let ended = store.summary_state(id).await?.ok_or("end state")?;
    assert_eq!(ended.input_revision, 2);
    assert_eq!(ended.last_input_at, active.last_input_at);
    assert_eq!(ended.unsettled_since, None);
    assert_eq!(
        ended.last_observed_end_at,
        Some("2026-01-03T01:08:00.123457Z".parse()?)
    );
    assert!(ended.last_observed_end_key.is_some());
    assert!(store.summary_eligible(id, 2).await?);
    store.capture(end.clone(), owner, None).await?;
    assert_eq!(store.summary_state(id).await?, Some(ended));

    let late = batch(KEY, vec![message("late", "2026-01-02T01:00:00Z")?]);
    store.capture(late, owner, None).await?;
    store.capture(end, owner, None).await?;
    assert!(!store.summary_eligible(id, 2).await?);
    assert!(!store.summary_eligible(id, 3).await?);
    let older_end = observed(
        batch(KEY, vec![]),
        "SessionEnd",
        "2026-01-03T01:08:00Z",
        true,
    )?;
    store.capture(older_end, owner, None).await?;
    assert!(!store.summary_eligible(id, 3).await?);

    let last = observed(
        batch(KEY, vec![message("last", "2026-01-03T01:10:00Z")?]),
        "SessionEnd",
        "2026-01-03T01:11:00Z",
        true,
    )?;
    store.capture(last, owner, None).await?;
    assert!(store.summary_eligible(id, 4).await?);

    assert_unknown_session_stays_absent(store, owner).await
}

async fn assert_unknown_session_stays_absent(
    store: &(impl MemoryStore + SummaryStore),
    owner: &Owner,
) -> TestResult {
    let ghost = observed(
        batch("claude-code:never-seen", vec![]),
        "SessionEnd",
        "2026-01-03T01:11:00Z",
        true,
    )?;
    assert_eq!(store.capture(ghost, owner, None).await?.conversation_id, "");
    assert!(
        store
            .mark_summary_settled("claude-code:never-seen", owner)
            .await?
            .is_none()
    );
    Ok(())
}

pub async fn position_fills<S: MemoryStore + SummaryStore + SessionCatalog>(
    store: &S,
    owner: &Owner,
) -> TestResult {
    const KEY: &str = "claude-code:settlement-order";
    let mut first = message("a", "2026-01-03T01:00:00Z")?;
    let mut second = message("b", "2026-01-03T01:01:00Z")?;
    second.metadata = Some(obj(json!({"uuid": null}))?);
    let original = batch(KEY, vec![first.clone(), second.clone()]);
    let id: Uuid = store
        .capture(original, owner, None)
        .await?
        .conversation_id
        .parse()?;
    store
        .mark_summary_settled(KEY, owner)
        .await?
        .ok_or("mark")?;

    first.metadata = Some(obj(json!({"session_jsonl_line": 2, "ordinal": 1}))?);
    store.capture(batch(KEY, vec![first]), owner, None).await?;
    assert!(store.summary_eligible(id, 1).await?);

    second.metadata = Some(obj(json!({"session_jsonl_line": 1}))?);
    store
        .capture(batch(KEY, vec![second.clone()]), owner, None)
        .await?;
    let changed = store.summary_state(id).await?.ok_or("reordered")?;
    assert_eq!(changed.input_revision, 2);
    assert!(changed.settled_at.is_none());
    let opening = store.opening_title(id).await?.ok_or("opening")?;
    assert_eq!(opening.content, second.content);
    assert_position_preserved(store, owner, second).await?;

    assert_eq!(store.summary_state(id).await?, Some(changed));
    Ok(())
}

async fn assert_position_preserved(
    store: &(impl MemoryStore + SummaryStore + SessionCatalog),
    owner: &Owner,
    mut second: CaptureMessage,
) -> TestResult {
    second.metadata = Some(obj(
        json!({"session_jsonl_line": 9, "assistant_uuid": "invented", "uuid": "invented"}),
    )?);
    store
        .capture(
            batch("claude-code:settlement-order", vec![second]),
            owner,
            None,
        )
        .await?;
    let page = store
        .messages(
            &"claude-code:settlement-order".parse()?,
            &MessagesQuery::new(None, None),
        )
        .await?;
    let metadata = page
        .items
        .first()
        .ok_or("first message")?
        .metadata
        .as_ref()
        .ok_or("position")?;
    assert_eq!(metadata.get("session_jsonl_line"), Some(&json!(1)));
    assert_eq!(metadata.get_str("assistant_uuid"), Some("invented"));
    assert_eq!(metadata.get("uuid"), Some(&json!(null)));
    Ok(())
}

pub async fn readers_agree(
    store: &(impl MemoryStore + SummaryStore + SessionCatalog),
    owner: &Owner,
    mut tie_timestamps: impl AsyncFnMut(Uuid) -> TestResult,
) -> TestResult {
    for mode in ["time", "line", "mixed"] {
        let key = format!("claude-code:tie-{mode}");
        let messages = tie_messages(mode)?;
        let id = store
            .capture(batch(&key, messages), owner, None)
            .await?
            .conversation_id
            .parse()?;
        tie_timestamps(id).await?;
        let mut tail = message("tail", "2026-01-03T01:00:03Z")?;
        tail.role = Role::Assistant;
        store.capture(batch(&key, vec![tail]), owner, None).await?;
        assert_reader_order(store, &key, id).await?;
    }
    Ok(())
}

fn tie_messages(mode: &str) -> Result<Vec<CaptureMessage>, Box<dyn std::error::Error>> {
    let mut messages = ["alpha", "beta", "gamma"]
        .map(|label| message(label, "2026-01-03T01:00:00Z"))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    if mode != "time" {
        for row in messages.iter_mut().take(if mode == "line" { 3 } else { 2 }) {
            row.metadata = Some(obj(
                json!({"session_jsonl_line": 3, "session_jsonl_path": "/invented/session.jsonl"}),
            )?);
        }
    }
    Ok(messages)
}

async fn assert_reader_order(
    store: &(impl MemoryStore + SummaryStore + SessionCatalog),
    key: &str,
    id: Uuid,
) -> TestResult {
    let page = store
        .messages(&key.parse()?, &MessagesQuery::new(None, None))
        .await?;
    let input = store.live_input(id).await?.ok_or("summarizer input")?;
    let window = store.conversation(id).await?.ok_or("window")?;
    let opening = store.opening_title(id).await?.ok_or("opening")?;
    let expected: Vec<_> = page.items.iter().map(|row| row.content.as_str()).collect();
    assert_eq!(expected.len(), 4);
    assert_eq!(
        input
            .messages
            .iter()
            .map(|row| row.content.as_str())
            .collect::<Vec<_>>(),
        expected,
        "{key}"
    );
    assert_eq!(
        window
            .messages
            .iter()
            .map(|row| row.content.as_str())
            .collect::<Vec<_>>(),
        expected,
        "{key}"
    );
    assert_eq!(
        opening.content,
        *expected.first().ok_or("initial message")?,
        "{key}"
    );
    Ok(())
}

pub async fn child_resume_signals(
    store: &(impl MemoryStore + SummaryStore),
    owner: &Owner,
) -> TestResult {
    for (event, settled) in [
        ("session.idle", false),
        ("session.execution.interrupted", false),
        ("session.execution.succeeded", true),
        ("session.execution.failed", true),
    ] {
        let key = format!("opencode:invented-child-{event}");
        let id = store
            .capture(
                batch(&key, vec![message("invented", "2026-01-01T00:00:00Z")?]),
                owner,
                None,
            )
            .await?
            .conversation_id
            .parse()?;
        let mut capture = batch(&key, vec![]);
        capture.finalize = Some(true);
        capture.settings = Some(obj(json!({
            "source": "opencode-hook",
            "parent_session_id": "invented-parent",
            "last_event": event,
            "last_ingest_at": "2026-01-01T00:00:01Z",
        }))?);
        store.capture(capture, owner, None).await?;
        assert_eq!(store.summary_eligible(id, 1).await?, settled, "{event}");
    }
    Ok(())
}

pub async fn expire<S: SummaryStore>(store: &S, from: Timestamp, limit: u32) -> TestResult {
    let mut after = None;
    loop {
        let page = store
            .expire_summary_page(after, from - SignedDuration::from_mins(1), from, limit)
            .await?;
        if page.next.is_none() {
            return Ok(());
        }
        assert!(page.next > after);
        after = page.next;
    }
}

pub async fn expiry_boundary(store: &impl SummaryStore, id: Uuid) -> TestResult {
    let input = store
        .summary_state(id)
        .await?
        .ok_or("live state")?
        .last_input_at;
    let now = input
        + SignedDuration::from_mins(i64::from(
            kernel::memory::settlement::DEFAULT_INACTIVITY_MINUTES,
        ));
    let just_before = input - SignedDuration::from_micros(1);
    assert_eq!(
        store
            .expire_summary_page(None, just_before, now, 10)
            .await?
            .settled,
        0
    );
    assert!(
        store
            .summary_state(id)
            .await?
            .ok_or("live state")?
            .settled_at
            .is_none()
    );
    assert_eq!(
        store
            .expire_summary_page(None, input, now, 10)
            .await?
            .settled,
        1
    );
    assert_eq!(
        store
            .summary_state(id)
            .await?
            .ok_or("live state")?
            .settled_at,
        Some(now)
    );
    Ok(())
}

pub fn pending_result_pair() -> Result<[CaptureMessage; 2], Box<dyn std::error::Error>> {
    let mut first = message("invented-tool", "2026-01-02T01:00:00Z")?;
    first.role = Role::Tool;
    first.tool_name = Some("invented-tool".to_owned());
    first.tool_args = Some(obj(json!({"fixture": "original"}))?);
    first.metadata = Some(obj(json!({
        "pending_result": true, "session_jsonl_line": 4, "tool_use_id": "invented"
    }))?);
    let mut done = first.clone();
    "Attempt to replace invented content.".clone_into(&mut done.content);
    done.tool_args = Some(obj(json!({"fixture": "replacement"}))?);
    done.created_at = Some("2026-01-02T01:01:00Z".parse()?);
    done.tool_result = Some("Invented result.".to_owned());
    done.metadata = Some(obj(json!({
        "pending_result": false, "tool_use_id": "replacement", "assistant_uuid": "invented"
    }))?);
    Ok([first, done])
}

pub fn cowork_pair() -> Result<[CaptureMessage; 2], Box<dyn std::error::Error>> {
    let mut hook = message("hook", "2026-01-02T01:00:00Z")?;
    hook.metadata = Some(obj(
        json!({"source": "cowork-hook", "ordinal": 1, "uuid": null}),
    )?);
    let mut transcript = hook.clone();
    "transcript".clone_into(&mut transcript.event_id);
    transcript.metadata = Some(obj(json!({
        "source": "cowork-transcript", "session_jsonl_line": 8, "ordinal": 9, "uuid": "invented"
    }))?);
    Ok([hook, transcript])
}

pub async fn replay_pending(
    store: &(impl MemoryStore + SummaryStore),
    owner: &Owner,
    completes_pending: bool,
) -> TestResult {
    let key = "claude-code:invented-tool";
    let [first, done] = pending_result_pair()?;
    let id = store
        .capture(batch(key, vec![first.clone()]), owner, None)
        .await?
        .conversation_id
        .parse()?;
    let mut empty = done.clone();
    empty.tool_result = Some(String::new());
    store.capture(batch(key, vec![empty]), owner, None).await?;
    for _ in 0..2 {
        assert_eq!(
            store
                .capture(batch(key, vec![done.clone()]), owner, None)
                .await?
                .inserted,
            0
        );
    }
    let input = store.live_input(id).await?.ok_or("summarizer input")?;
    let [row] = input.messages.as_slice() else {
        return Err("expected one stored event".into());
    };
    assert_eq!(row.role, first.role);
    assert_eq!(row.content, first.content);
    assert_eq!(row.created_at, first.created_at);
    assert_eq!(row.tool_args, first.tool_args);
    assert_eq!(
        row.tool_result,
        if completes_pending {
            done.tool_result
        } else {
            None
        }
    );
    let metadata = row.metadata.as_ref().ok_or("stored metadata")?;
    assert_eq!(metadata.get("session_jsonl_line"), Some(&json!(4)));
    assert_eq!(metadata.get_str("tool_use_id"), Some("invented"));
    assert_eq!(metadata.get_str("assistant_uuid"), Some("invented"));
    Ok(())
}

pub async fn precise_pages(store: &impl SessionCatalog, key: &str) -> TestResult {
    let key = key.parse()?;
    let mut after = None;
    for label in ["second", "earlier", "later", "offset"] {
        let page = store
            .messages(&key, &MessagesQuery::new(after, Some(1)))
            .await?;
        assert_eq!(page.total, 4);
        assert_eq!(page.items.len(), 1, "{label}");
        assert_eq!(
            page.items.first().ok_or("page message")?.content,
            format!("Inspect the invented sample {label}.")
        );
        after = page
            .next
            .map(|cursor| kernel::memory::MessageCursor::decode(&cursor.encode()))
            .transpose()?;
    }
    assert!(after.is_none());
    Ok(())
}

pub fn precision_messages() -> Result<Vec<CaptureMessage>, jiff::Error> {
    [
        ("later", "2026-01-01T00:00:01.123900Z"),
        ("second", "2026-01-01T00:00:01Z"),
        ("offset", "2026-01-01T01:00:01.130+01:00"),
        ("earlier", "2026-01-01T00:00:01.123100Z"),
    ]
    .into_iter()
    .map(|(label, at)| message(label, at))
    .collect()
}
