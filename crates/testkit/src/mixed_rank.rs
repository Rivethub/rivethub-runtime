use std::collections::BTreeSet;

use jiff::Timestamp;
use kernel::memory::BrowseFilter;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::EmbedTable;
use kernel::memory::EmbedTarget;
use kernel::memory::EmbeddingStore;
use kernel::memory::Limit;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::MemoryEntry;
use kernel::memory::MemoryScope;
use kernel::memory::MemoryStore;
use kernel::memory::SearchMode;
use kernel::memory::SearchQuery;
use kernel::memory::StoredChunk;
use kernel::memory::StoredEmbedding;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryStore;
use kernel::memory::jobs::MemoryJobStore;
use kernel::memory::summary_backend::SummaryIndex;
use kernel::memory::summary_backend::SummaryPassage;
use kernel::memory::summary_reference::PreparedSummaryRecord;
use kernel::memory::summary_reference::PreparedSummaryTree;
use kernel::memory::summary_reference::SummaryManifestEntry;
use kernel::memory::summary_reference::SummarySectionBounds;
use uuid::Uuid;
use wire::message::Role;

use crate::summary_passages::vector;
use crate::topics::Outcome;

pub const EQUAL_CHUNK_DISTANCES_AT_THE_CUTOFF: &str = "Equal chunk distances at the pool cutoff stay index-ordered on Postgres and exact top-k on SQLite. Fixtures avoid ties at the cutoff.";

const AT: &str = "2026-06-01T00:00:00Z";
const SESSION_KEY: &str = "rank-mixed";
const PARENT_TEXT: &str = "ordinary orchard message";
const CHUNK_TEXT: &str = "chunk orchard message";
const PASSAGE_TEXT: &str = "orchard summary passage";

struct Seeded {
    parent: Uuid,
    chunk_parent: Uuid,
    summary: Uuid,
}

pub async fn a_message_a_chunk_and_a_passage_rank_together<S>(store: &S) -> Outcome
where
    S: MemoryStore + EmbeddingStore + MemoryJobStore + SummaryStore,
{
    let seeded = seed(store).await?;
    assert_eq!(ranked(store).await?, seeded.spread());
    Ok(())
}

pub async fn a_near_tie_parent_outranks_the_passage<S>(store: &S) -> Outcome
where
    S: MemoryStore + EmbeddingStore + MemoryJobStore + SummaryStore,
{
    let seeded = seed(store).await?;
    store
        .write(stored(seeded.parent, vector(1.0005, 0.7504)))
        .await?;
    assert_eq!(ranked(store).await?, seeded.near());
    Ok(())
}

impl Seeded {
    fn spread(&self) -> Vec<Hit> {
        vec![
            hit(&self.chunk_parent, Some(0), false),
            hit(&self.summary, None, true),
            hit(&self.parent, None, false),
        ]
    }

    fn near(&self) -> Vec<Hit> {
        vec![
            hit(&self.chunk_parent, Some(0), false),
            hit(&self.parent, None, false),
            hit(&self.summary, None, true),
        ]
    }
}

type Hit = (String, Option<u32>, bool);

fn hit(id: &Uuid, chunk: Option<u32>, passage: bool) -> Hit {
    (id.to_string(), chunk, passage)
}

async fn seed<S>(store: &S) -> Result<Seeded, Box<dyn std::error::Error>>
where
    S: MemoryStore + EmbeddingStore + MemoryJobStore + SummaryStore,
{
    let saved = capture(store).await?;
    store
        .mark_summary_settled(SESSION_KEY, &crate::owner()?)
        .await?
        .ok_or("the session did not settle")?;
    store.write(stored(saved.parent, vector(3.0, 4.0))).await?;
    store
        .write_chunks(saved.chunk_parent, vec![chunk_span()])
        .await?;
    let summary = Uuid::from_u128(0x3100);
    let published = store
        .publish_live(
            tree(saved.conversation, saved.parent, summary)?,
            [(summary, passage_index()?)].into(),
        )
        .await?;
    assert!(published);
    Ok(Seeded {
        parent: saved.parent,
        chunk_parent: saved.chunk_parent,
        summary,
    })
}

async fn ranked<S: MemoryStore>(store: &S) -> Result<Vec<Hit>, Box<dyn std::error::Error>> {
    let outcome = store
        .search(SearchQuery {
            text: "orchard".to_owned(),
            mode: SearchMode::Vector,
            scope: MemoryScope::Both,
            agent: None,
            role: None,
            exclude_roles: Vec::new(),
            tool_name: None,
            tag: None,
            limit: Limit::clamped(Some(10), 10, 1..=50),
            since: None,
            before: None,
            window: None,
            expand: Some(false),
            synthesize: None,
        })
        .await?;
    assert_eq!(outcome.degraded, None);
    Ok(outcome
        .results
        .iter()
        .map(|row| {
            (
                row.id.clone(),
                row.chunk.as_ref().map(|chunk| chunk.index),
                row.passage.is_some(),
            )
        })
        .collect())
}

struct Saved {
    conversation: Uuid,
    parent: Uuid,
    chunk_parent: Uuid,
}

async fn capture<S: MemoryStore>(store: &S) -> Result<Saved, Box<dyn std::error::Error>> {
    let at = stamp()?;
    let captured = store.capture(batch(at), &crate::owner()?, None).await?;
    let conversation = Uuid::parse_str(&captured.conversation_id)?;
    let rows = store.browse(filter(conversation)).await?;
    Ok(Saved {
        conversation,
        parent: message_id(&rows, PARENT_TEXT)?,
        chunk_parent: message_id(&rows, CHUNK_TEXT)?,
    })
}

fn message_id(rows: &[MemoryEntry], content: &str) -> Result<Uuid, Box<dyn std::error::Error>> {
    let entry = rows
        .iter()
        .find(|row| row.content == content)
        .ok_or("the captured message is missing")?;
    let id = entry.id.clone().ok_or("the captured message has no id")?;
    Ok(Uuid::parse_str(&id)?)
}

fn batch(at: Timestamp) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: SESSION_KEY.to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("cli".to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: Some(at),
        updated_at: Some(at),
        messages: vec![message(PARENT_TEXT, at), message(CHUNK_TEXT, at)],
    }
}

fn message(content: &str, at: Timestamp) -> CaptureMessage {
    CaptureMessage {
        event_id: Uuid::new_v4().to_string(),
        role: Role::User,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(at),
    }
}

fn filter(conversation: Uuid) -> BrowseFilter {
    BrowseFilter {
        conversation_id: Some(conversation),
        agent: None,
        role: Some(Role::User),
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

fn stored(id: Uuid, vector: Vec<f32>) -> StoredEmbedding {
    StoredEmbedding {
        target: EmbedTarget {
            table: EmbedTable::Messages,
            id: id.to_string(),
        },
        vector,
        content_hash: "rank".to_owned(),
        chunks: Vec::new(),
    }
}

fn chunk_span() -> StoredChunk {
    StoredChunk {
        index: 0,
        char_start: 0,
        char_end: 6,
        content: "chunk".to_owned(),
        vector: Some(vector(0.5, 0.0)),
    }
}

fn passage_index() -> Result<SummaryIndex, Box<dyn std::error::Error>> {
    let at = stamp()?;
    Ok(SummaryIndex {
        pooled: vector(4.0, 3.0),
        passages: vec![SummaryPassage {
            content: PASSAGE_TEXT.to_owned(),
            chars: 0..passage_end()?,
            earliest_at: at,
            latest_at: at,
            vector: vector(4.0, 3.0),
        }],
    })
}

fn tree(
    conversation: Uuid,
    message: Uuid,
    summary: Uuid,
) -> Result<PreparedSummaryTree, Box<dyn std::error::Error>> {
    let section = Uuid::from_u128(0x3101);
    Ok(PreparedSummaryTree {
        conversation,
        owner: crate::owner()?.as_str().to_owned(),
        expected_ids: BTreeSet::new(),
        records: vec![
            session_record(summary, message),
            section_record(section, summary)?,
        ],
        retained_parts: BTreeSet::new(),
        part_order: Vec::new(),
        historical: false,
        expected_revision: Some(1),
        passage_windows: [(summary, std::iter::once(0..passage_end()?).collect())].into(),
        created_at: stamp()?,
    })
}

fn session_record(summary: Uuid, message: Uuid) -> PreparedSummaryRecord {
    PreparedSummaryRecord {
        id: summary,
        parent: None,
        ordinal: 0,
        kind: SummaryKind::Session,
        content: PASSAGE_TEXT.to_owned(),
        input_key: [1; 32],
        index_key: Some([0xab; 32]),
        model: None,
        issued_part_ordinal: None,
        input_tokens: None,
        quality_flags: 0,
        section: None,
        sources: vec![SummaryManifestEntry {
            message,
            ordinal: 1,
            render_key: None,
        }],
    }
}

fn section_record(
    id: Uuid,
    parent: Uuid,
) -> Result<PreparedSummaryRecord, Box<dyn std::error::Error>> {
    Ok(PreparedSummaryRecord {
        id,
        parent: Some(parent),
        ordinal: 1,
        kind: SummaryKind::Section,
        content: "## Orchard\nsectionunique".to_owned(),
        input_key: [1; 32],
        index_key: None,
        model: None,
        issued_part_ordinal: None,
        input_tokens: None,
        quality_flags: 0,
        section: Some(SummarySectionBounds {
            first: 1,
            last: 1,
            verified: true,
            char_start: 0,
            char_end: passage_end()?,
        }),
        sources: Vec::new(),
    })
}

fn passage_end() -> Result<i32, Box<dyn std::error::Error>> {
    Ok(i32::try_from(PASSAGE_TEXT.chars().count())?)
}

fn stamp() -> Result<Timestamp, jiff::Error> {
    AT.parse()
}
