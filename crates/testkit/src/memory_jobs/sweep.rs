use kernel::memory::MemoryStore;
use kernel::memory::NewSummary;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryStore;
use kernel::memory::jobs::MemoryJobError;
use kernel::memory::jobs::MemoryJobStore;
use kernel::memory::jobs::SynthesizeToolCall;
use kernel::queue::ClaimSelector;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::wiki::builder::ExtractWiki;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;

use super::capture;
use super::message;
use super::noted;
use super::user;
use crate::topics::Outcome;
use crate::topics::same;

pub async fn a_tool_sentence_replaces_only_matching_content<S>(store: &S) -> Outcome
where
    S: MemoryStore + MemoryJobStore,
{
    let mut args = JsonObject::default();
    args.insert("path", "notes.txt")?;
    let id = message(
        store,
        noted("tools", Role::Assistant, "pending").tool("Bash", Some(args.clone()), None),
    )
    .await?;
    let loaded = store
        .tool_call(id)
        .await?
        .ok_or("the tool call is missing")?;
    assert_eq!(loaded.tool_name.as_deref(), Some("Bash"));
    assert_eq!(loaded.tool_args, args);
    assert_eq!(loaded.content, "pending");
    store.write_tool_sentence(id, "nope", "replaced").await?;
    let unchanged = store
        .tool_call(id)
        .await?
        .ok_or("the tool call is missing")?;
    assert_eq!(unchanged.content, "pending");
    store
        .write_tool_sentence(id, "pending", "the tool listed the notes")
        .await?;
    let updated = store
        .tool_call(id)
        .await?
        .ok_or("the tool call is missing")?;
    assert_eq!(updated.content, "the tool listed the notes");
    let bare = message(
        store,
        noted("tools", Role::Assistant, "bare").tool("Bash", None, None),
    )
    .await?;
    let bare = store
        .tool_call(bare)
        .await?
        .ok_or("the bare tool call is missing")?;
    assert_eq!(bare.tool_args, JsonObject::default());
    Ok(())
}

pub async fn tool_arguments_match<S>(store: &S, id: Uuid, path: &str) -> Outcome
where
    S: MemoryJobStore,
{
    let loaded = store
        .tool_call(id)
        .await?
        .ok_or("the tool call is missing")?;
    let mut want = JsonObject::default();
    want.insert("path", path)?;
    assert_eq!(loaded.tool_args, want);
    Ok(())
}

pub async fn tool_arguments_are_rejected<S>(store: &S, id: Uuid) -> Outcome
where
    S: MemoryJobStore,
{
    match store.tool_call(id).await {
        Err(MemoryJobError::Arguments(_)) => Ok(()),
        other => Err(format!("tool arguments returned {other:?}").into()),
    }
}

pub async fn a_non_heartbeat_leaf_is_the_only_missing_wiki<S, Q>(store: &S, queue: &Q) -> Outcome
where
    S: MemoryStore + MemoryJobStore + SummaryStore,
    Q: JobQueue,
{
    let notes = capture(store, user("claude-code:notes", "notes")).await?;
    let pulse = capture(store, user("heartbeat:pulse", "pulse")).await?;
    let leaf = summary(
        store,
        notes.conversation,
        SummaryKind::Leaf,
        "the notes leaf",
    )
    .await?;
    summary(
        store,
        pulse.conversation,
        SummaryKind::Leaf,
        "the pulse leaf",
    )
    .await?;
    summary(
        store,
        notes.conversation,
        SummaryKind::Branch,
        "the notes branch",
    )
    .await?;
    let missing = store.missing_wiki(10).await?;
    let ids: Vec<Uuid> = missing.iter().map(|row| row.job.summary_id).collect();
    same("missing wiki", ids, vec![leaf])?;
    queue
        .enqueue(wiki_job(leaf, Some(notes.conversation), 2)?)
        .await?;
    assert_eq!(store.missing_wiki(10).await?, Vec::new());
    Ok(())
}

pub async fn a_fresh_dead_wiki_job_is_not_revived<S, Q>(store: &S, queue: &Q) -> Outcome
where
    S: MemoryJobStore,
    Q: JobQueue,
{
    assert_eq!(store.revive_dead_wiki(10).await?, 0);
    let summary = Uuid::new_v4();
    queue.enqueue(wiki_job(summary, None, 1)?).await?;
    let selector = ClaimSelector::matching(vec![JobName::Fixed(FixedJob::ExtractWiki)]);
    let claimed = queue
        .claim(&selector)
        .await?
        .ok_or("the wiki job was not claimed")?;
    queue.fail(claimed.id, "boom").await?;
    assert_eq!(store.revive_dead_wiki(10).await?, 0);
    assert_eq!(queue.claim(&selector).await?, None);
    Ok(())
}

pub async fn an_empty_assistant_tool_call_is_backfilled_once<S, Q, F>(
    store: &S,
    queue: &Q,
    mut backfill: F,
) -> Outcome
where
    S: MemoryStore,
    Q: JobQueue,
    F: AsyncFnMut(Option<u32>, bool) -> Result<u64, Box<dyn std::error::Error + Send + Sync>>,
{
    let id = message(
        store,
        noted("synth", Role::Assistant, "   ").tool("Bash", None, None),
    )
    .await?;
    message(
        store,
        noted("synth", Role::User, "   ").tool("Bash", None, None),
    )
    .await?;
    message(
        store,
        noted("synth", Role::Assistant, "hello").tool("Bash", None, None),
    )
    .await?;
    let selector = ClaimSelector::matching(vec![JobName::Fixed(FixedJob::SynthesizeToolCall)]);
    assert_eq!(backfill(None, true).await.map_err(widen)?, 1);
    assert_eq!(queue.claim(&selector).await?, None);
    assert_eq!(backfill(None, false).await.map_err(widen)?, 1);
    let claimed = queue
        .claim(&selector)
        .await?
        .ok_or("the synthesize job was not claimed")?;
    let text = serde_json::to_string(&claimed.payload)?;
    let body: SynthesizeToolCall = serde_json::from_str(&text)?;
    assert_eq!(body.message_id, id);
    assert_eq!(backfill(None, false).await.map_err(widen)?, 0);
    queue.complete(claimed.id).await?;
    assert_eq!(queue.claim(&selector).await?, None);
    Ok(())
}

pub async fn remember_tool_call<S>(store: &S) -> Result<Uuid, Box<dyn std::error::Error>>
where
    S: MemoryStore,
{
    message(
        store,
        noted("encoded", Role::Assistant, "encoded").tool("Bash", None, None),
    )
    .await
}

fn widen(error: Box<dyn std::error::Error + Send + Sync>) -> Box<dyn std::error::Error> {
    error
}

fn wiki_job(
    summary_id: Uuid,
    conversation_id: Option<Uuid>,
    max_attempts: i32,
) -> Result<Job, serde_json::Error> {
    Ok(Job {
        name: JobName::Fixed(FixedJob::ExtractWiki),
        payload: JsonObject::from_serialize(&ExtractWiki {
            summary_id,
            conversation_id,
        })?,
        key: Some(format!("wiki-ext-{summary_id}")),
        key_mode: JobKeyMode::KeepRunAt,
        priority: 0,
        run_at: None,
        max_attempts: Some(max_attempts),
    })
}

async fn summary<S: SummaryStore>(
    store: &S,
    conversation: Uuid,
    kind: SummaryKind,
    content: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let id = Uuid::new_v4();
    let at = wire::time::parse_rfc3339("2026-06-01T00:00:00Z")?;
    store
        .write_summary(NewSummary {
            id,
            conversation_id: conversation,
            kind,
            depth: kind.depth(),
            content: content.to_owned(),
            message_count: 1,
            earliest_at: at,
            latest_at: at,
            model: "test".to_owned(),
            pipeline_version: 1,
            created_at: at,
            sources: Vec::new(),
            child_ids: Vec::new(),
        })
        .await?;
    Ok(id)
}
