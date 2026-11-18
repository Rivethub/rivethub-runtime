use chrono::{DateTime, TimeDelta, Utc};
use kernel::memory::settlement::SettleStatus;
use kernel::memory::{CaptureBatchRequest, CaptureMessage, MemoryStore, SummaryStore};
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

pub fn message(id: &str, at: &str) -> Result<CaptureMessage, chrono::ParseError> {
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

pub async fn revision_lifecycle<S: MemoryStore + SummaryStore>(
    store: &S,
    owner: &Owner,
) -> TestResult {
    const KEY: &str = "claude-code:settlement-fixture";
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

    let two = message("two", "2026-01-03T01:01:00Z")?;
    let reopen = observed(batch(KEY, vec![two]), "Stop", "2026-01-03T01:08:00Z", false)?;
    store.capture(reopen, owner, None).await?;
    let active = store.summary_state(id).await?.ok_or("reopened state")?;
    assert_eq!(active.input_revision, 2);
    assert_eq!(active.unsettled_since, Some(active.last_input_at));
    assert!(active.last_input_at >= initial.last_input_at);
    assert!(!store.summary_eligible(id, 1).await?);
    assert!(!store.summary_eligible(id, 2).await?);

    let superseded = observed(
        batch(KEY, vec![]),
        "SessionEnd",
        "2026-01-03T01:07:00Z",
        true,
    )?;
    store.capture(superseded, owner, None).await?;
    assert_eq!(store.summary_state(id).await?, Some(active.clone()));

    let end = observed(
        batch(KEY, vec![]),
        "SessionEnd",
        "2026-01-03T01:09:00Z",
        true,
    )?;
    store.capture(end.clone(), owner, None).await?;
    let ended = store.summary_state(id).await?.ok_or("end state")?;
    assert_eq!(ended.input_revision, 2);
    assert_eq!(ended.last_input_at, active.last_input_at);
    assert_eq!(ended.unsettled_since, None);
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
        "2026-01-03T01:08:30Z",
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

    let ghost = observed(
        batch("claude-code:never-seen", vec![]),
        "SessionEnd",
        "2026-01-03T01:11:00Z",
        true,
    )?;
    assert!(
        store
            .capture(ghost, owner, None)
            .await?
            .conversation_id
            .is_empty()
    );
    assert!(
        store
            .mark_summary_settled("claude-code:never-seen", owner)
            .await?
            .is_none()
    );
    Ok(())
}

pub async fn position_fills<S: MemoryStore + SummaryStore>(store: &S, owner: &Owner) -> TestResult {
    const KEY: &str = "cli:settlement-order";
    let mut first = message("a", "2026-01-03T01:00:00Z")?;
    let mut second = message("b", "2026-01-03T01:01:00Z")?;
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

    first.metadata = Some(obj(json!({"session_jsonl_line": 2}))?);
    store.capture(batch(KEY, vec![first]), owner, None).await?;
    assert!(store.summary_eligible(id, 1).await?);

    second.metadata = Some(obj(json!({"session_jsonl_line": 1}))?);
    store
        .capture(batch(KEY, vec![second.clone()]), owner, None)
        .await?;
    let changed = store.summary_state(id).await?.ok_or("reordered")?;
    assert_eq!(changed.input_revision, 2);
    assert!(changed.settled_at.is_none());
    store.capture(batch(KEY, vec![second]), owner, None).await?;
    assert_eq!(store.summary_state(id).await?, Some(changed));
    Ok(())
}

pub async fn expire<S: SummaryStore>(store: &S, from: DateTime<Utc>, limit: u32) -> TestResult {
    let mut after = None;
    loop {
        let page = store
            .expire_summary_page(after, from - TimeDelta::minutes(1), from, limit)
            .await?;
        if page.next.is_none() {
            return Ok(());
        }
        assert!(page.next > after);
        after = page.next;
    }
}
