use jiff::Timestamp;
use kernel::memory::BrowseFilter;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::EmbeddingStore;
use kernel::memory::Limit;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::MemoryEntry;
use kernel::memory::MemoryScope;
use kernel::memory::MemoryStore;
use kernel::memory::SearchMode;
use kernel::memory::SearchQuery;
use kernel::memory::SessionDeletion;
use kernel::memory::StoredChunk;
use kernel::memory::TagAdd;
use kernel::memory::TagEntity;
use kernel::memory::TagEntityType;
use kernel::memory::TagLiteral;
use kernel::memory::jobs::MemoryJobStore;
use uuid::Uuid;
use wire::message::Role;

use crate::summary_passages::vector;
use crate::topics::Outcome;

const KEPT: &str = "the kept orchard note is long enough to stay in a hybrid search result";
const DROPPED: &str = "the dropped orchard note is long enough to stay in a hybrid search result";
const NOW: &str = "2026-06-01T00:00:00Z";
const EARLIER: &str = "2020-01-01T00:00:00Z";
const WINDOW: &str = "2024-01-01T00:00:00Z";

type Hit = (String, Option<u32>);

pub async fn a_trashed_session_loses_its_chunk<S>(store: &S) -> Outcome
where
    S: MemoryStore + EmbeddingStore + MemoryJobStore + SessionDeletion,
{
    let kept = speak(store, "trash-kept", Role::User, KEPT, NOW).await?;
    let dropped = speak(store, "trash-dropped", Role::User, DROPPED, NOW).await?;
    attach(store, kept, false).await?;
    attach(store, dropped, true).await?;
    store
        .trash_session("trash-dropped", None, stamp(NOW)?)
        .await?;
    assert_eq!(
        hits(store, query(SearchMode::Vector)).await?,
        kept_hit(kept)
    );
    Ok(())
}

pub async fn a_role_filter_drops_the_other_parents_chunk<S>(store: &S) -> Outcome
where
    S: MemoryStore + EmbeddingStore + MemoryJobStore,
{
    let kept = speak(store, "role-pair", Role::User, KEPT, NOW).await?;
    let dropped = speak(store, "role-pair", Role::Assistant, DROPPED, NOW).await?;
    attach(store, kept, false).await?;
    attach(store, dropped, true).await?;
    let mut query = query(SearchMode::Vector);
    query.role = Some(Role::User);
    assert_eq!(hits(store, query).await?, kept_hit(kept));
    Ok(())
}

pub async fn a_time_window_drops_an_older_chunk<S>(store: &S) -> Outcome
where
    S: MemoryStore + EmbeddingStore + MemoryJobStore,
{
    let kept = speak(store, "time-pair", Role::User, KEPT, NOW).await?;
    let dropped = speak(store, "time-pair", Role::User, DROPPED, EARLIER).await?;
    attach(store, kept, false).await?;
    attach(store, dropped, true).await?;
    let mut query = query(SearchMode::Vector);
    query.since = Some(stamp(WINDOW)?);
    assert_eq!(hits(store, query).await?, kept_hit(kept));
    Ok(())
}

pub async fn said_only_drops_a_tool_parents_chunk<S>(store: &S) -> Outcome
where
    S: MemoryStore + EmbeddingStore + MemoryJobStore,
{
    let kept = speak(store, "said-pair", Role::User, KEPT, NOW).await?;
    let dropped = speak(store, "said-pair", Role::Tool, DROPPED, NOW).await?;
    attach(store, kept, false).await?;
    attach(store, dropped, true).await?;
    let mut query = query(SearchMode::Vector);
    query.exclude_roles = vec![Role::Tool, Role::System];
    assert_eq!(hits(store, query).await?, kept_hit(kept));
    Ok(())
}

pub async fn a_hybrid_tag_keeps_the_tagged_conversations_chunk<S>(store: &S) -> Outcome
where
    S: MemoryStore + EmbeddingStore + MemoryJobStore,
{
    let kept = speak(store, "tag-kept", Role::User, KEPT, NOW).await?;
    let dropped = speak(store, "tag-dropped", Role::User, DROPPED, NOW).await?;
    tag(store).await?;
    attach(store, kept, false).await?;
    attach(store, dropped, true).await?;
    let mut query = query(SearchMode::Hybrid);
    query.tag = Some(TagLiteral {
        key: "project".to_owned(),
        value: "orchard".to_owned(),
    });
    assert_eq!(hits(store, query).await?, kept_hit(kept));
    Ok(())
}

pub async fn clearing_chunk_vectors_drops_the_hit<S>(
    store: &S,
    clear: impl AsyncFnOnce() -> Outcome,
) -> Outcome
where
    S: MemoryStore + EmbeddingStore + MemoryJobStore,
{
    let kept = speak(store, "clear-kept", Role::User, KEPT, NOW).await?;
    attach(store, kept, true).await?;
    assert_eq!(
        hits(store, query(SearchMode::Vector)).await?,
        kept_hit(kept)
    );
    clear().await?;
    assert_eq!(
        hits(store, query(SearchMode::Vector)).await?,
        Vec::<Hit>::new()
    );
    Ok(())
}

fn kept_hit(id: Uuid) -> Vec<Hit> {
    vec![(id.to_string(), Some(0))]
}

async fn tag<S: MemoryStore>(store: &S) -> Outcome {
    store
        .add_tag(TagAdd {
            entity_type: TagEntityType::Conversation,
            entity: TagEntity::SessionKey("tag-kept".to_owned()),
            agent: None,
            tag: TagLiteral {
                key: "project".to_owned(),
                value: "orchard".to_owned(),
            },
            display: None,
            reason: None,
            decided_by: None,
        })
        .await?;
    Ok(())
}

async fn attach<S: MemoryJobStore>(store: &S, id: Uuid, close: bool) -> Outcome {
    store.write_chunks(id, vec![chunk(close)]).await?;
    Ok(())
}

async fn hits<S: MemoryStore>(
    store: &S,
    query: SearchQuery,
) -> Result<Vec<Hit>, Box<dyn std::error::Error>> {
    let outcome = store.search(query).await?;
    assert_eq!(outcome.degraded, None);
    Ok(outcome
        .results
        .into_iter()
        .map(|row| (row.id, row.chunk.map(|chunk| chunk.index)))
        .collect())
}

fn query(mode: SearchMode) -> SearchQuery {
    SearchQuery {
        text: "zzzzqqqx".to_owned(),
        mode,
        scope: MemoryScope::Messages,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(5), 10, 1..=50),
        since: None,
        before: None,
        window: None,
        expand: Some(false),
        synthesize: None,
    }
}

fn chunk(close: bool) -> StoredChunk {
    let (first, second) = if close { (1.0, 0.0) } else { (0.6, 0.8) };
    StoredChunk {
        index: 0,
        char_start: 0,
        char_end: 4,
        content: "note".to_owned(),
        vector: Some(vector(first, second)),
    }
}

async fn speak<S: MemoryStore>(
    store: &S,
    session: &str,
    role: Role,
    content: &str,
    at: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let at = stamp(at)?;
    let captured = store
        .capture(batch(session, role, content, at), &crate::owner()?, None)
        .await?;
    let conversation = Uuid::parse_str(&captured.conversation_id)?;
    let rows = store.browse(filter(conversation)).await?;
    message_id(&rows, content)
}

fn batch(session: &str, role: Role, content: &str, at: Timestamp) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: session.to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("cli".to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: Some(at),
        updated_at: Some(at),
        messages: vec![message(role, content, at)],
    }
}

fn message(role: Role, content: &str, at: Timestamp) -> CaptureMessage {
    let tool = role == Role::Tool;
    CaptureMessage {
        event_id: Uuid::new_v4().to_string(),
        role,
        content: content.to_owned(),
        tool_name: tool.then(|| "Grep".to_owned()),
        tool_args: None,
        tool_result: tool.then(|| "match".to_owned()),
        metadata: None,
        created_at: Some(at),
    }
}

fn filter(conversation: Uuid) -> BrowseFilter {
    BrowseFilter {
        conversation_id: Some(conversation),
        agent: None,
        role: None,
        tool_name: None,
        tag: None,
        window: None,
        since: None,
        before: None,
        include_tools: true,
        order: MemoryBrowseOrder::Desc,
        limit: Limit::clamped(Some(20), 50, 1..=200),
    }
}

fn message_id(rows: &[MemoryEntry], content: &str) -> Result<Uuid, Box<dyn std::error::Error>> {
    let entry = rows
        .iter()
        .find(|row| row.content == content)
        .ok_or("the captured message is missing")?;
    let id = entry.id.clone().ok_or("the captured message has no id")?;
    Ok(Uuid::parse_str(&id)?)
}

fn stamp(text: &str) -> Result<Timestamp, jiff::Error> {
    text.parse()
}
