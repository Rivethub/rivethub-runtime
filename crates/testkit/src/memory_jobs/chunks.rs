use jiff::SignedDuration;
use kernel::memory::CaptureMessage;
use kernel::memory::EmbedTable;
use kernel::memory::EmbedTarget;
use kernel::memory::EmbeddingStore;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::MemoryChunkMatch;
use kernel::memory::MemoryScope;
use kernel::memory::MemoryStore;
use kernel::memory::SearchMode;
use kernel::memory::SearchQuery;
use kernel::memory::StoredChunk;
use kernel::memory::StoredEmbedding;
use kernel::memory::Unembeddable;
use kernel::memory::jobs::ChunkWrite;
use kernel::memory::jobs::MemoryJobStore;
use kernel::memory::jobs::UnembeddedStore;
use uuid::Uuid;
use wire::message::Role;

use super::axis;
use super::browse;
use super::message;
use super::pair;
use super::query_vector;
use super::user;
use crate::topics::Outcome;
use crate::topics::same;

const CHUNK_ONLY: &str = "banana cultivation notes from the AcmeKit greenhouse rotation";
const ROLLBACK: &str = "rollback drills monthly so the staging deploys always recover";
const DEPLOY: &str = "deploy scripts run nightly before the staging health check gate";
const CHUNK_TEXT: &str = "seed chunk";

pub async fn a_phrase_inside_a_long_message_outranks_its_parent<S>(store: &S) -> Outcome
where
    S: MemoryStore + MemoryJobStore + EmbeddingStore,
{
    let chunk_only = message(store, user("chunk-only", CHUNK_ONLY)).await?;
    let rollback = message(store, user("rollback", ROLLBACK)).await?;
    let deploy = message(store, user("deploy", DEPLOY)).await?;
    write_parent(store, chunk_only, pair(1.0, 1.0)).await?;
    write_parent(store, rollback, pair(3.0, 1.0)).await?;
    write_parent(store, deploy, axis(1, 1.0)).await?;
    write_span(store, chunk_only, &pair(0.5, 0.0)).await?;
    let outcome = store.search(vector_query("banana")).await?;
    assert_eq!(outcome.degraded, None);
    let contents: Vec<&str> = outcome
        .results
        .iter()
        .map(|hit| hit.content.as_str())
        .collect();
    assert_eq!(contents, vec![CHUNK_ONLY, ROLLBACK, DEPLOY]);
    assert_eq!(hit(&outcome.results, CHUNK_ONLY)?.chunk, Some(span()));
    assert_eq!(hit(&outcome.results, ROLLBACK)?.chunk, None);
    assert_eq!(hit(&outcome.results, DEPLOY)?.chunk, None);
    decreasing_scores(&outcome.results)?;
    Ok(())
}

pub async fn unchunked_counts_trimmed_characters_and_caps_tool_output<S>(store: &S) -> Outcome
where
    S: MemoryStore + MemoryJobStore + EmbeddingStore,
{
    let fixtures = fixtures();
    let ids = capture_fixtures(store, &fixtures).await?;
    mark_fixtures(store, &fixtures, &ids).await?;
    for case in cases() {
        let rows = store.unchunked(case.limit, case.chars, case.cap).await?;
        let got: Vec<(&str, Option<&str>)> = rows
            .iter()
            .map(|row| (row.content.as_str(), row.tool_result.as_deref()))
            .collect();
        assert_eq!(
            got,
            expected(&fixtures, case.minutes)?,
            "{} {} {}",
            case.chars,
            case.cap,
            case.limit
        );
    }
    Ok(())
}

pub async fn chunks_are_written_once_and_an_unknown_message_is_skipped<S>(store: &S) -> Outcome
where
    S: MemoryStore + MemoryJobStore,
{
    let first = message(store, user("chunks", "first body")).await?;
    let second = message(store, user("chunks", "second body")).await?;
    assert_eq!(
        store.write_chunks(first, vec![open_span(0, "x")]).await?,
        ChunkWrite::Written
    );
    assert_eq!(
        store.write_chunks(first, Vec::new()).await?,
        ChunkWrite::Skipped
    );
    assert_eq!(
        store.write_chunks(Uuid::from_u128(999), Vec::new()).await?,
        ChunkWrite::Skipped
    );
    assert_eq!(
        store.write_chunks(second, Vec::new()).await?,
        ChunkWrite::Written
    );
    Ok(())
}

pub async fn an_unembedded_chunk_is_not_healed_and_clears_once_written<S>(store: &S) -> Outcome
where
    S: MemoryStore + MemoryJobStore + EmbeddingStore + UnembeddedStore,
{
    let kept = message(store, user("embed", "kept body")).await?;
    let blank = message(store, user("embed", "blank body")).await?;
    let heal = message(store, user("embed", "heal body")).await?;
    store
        .write_chunks(kept, vec![unit_span(0, "first"), open_span(1, "second")])
        .await?;
    store.write_chunks(blank, vec![open_span(0, "   ")]).await?;
    store
        .write_chunks(heal, vec![open_span(0, "heal-me")])
        .await?;
    assert_eq!(
        loaded_content(store, &only(store.unembedded_chunks(kept).await?)?).await?,
        "second"
    );
    let heal_id = only(store.unembedded_chunks(heal).await?)?;
    store
        .mark_permanent(&chunk_target(&heal_id), "stuck")
        .await?;
    let sweep = store.unembedded(EmbedTable::MessageChunks, 10).await?;
    let mut contents = Vec::new();
    for target in &sweep {
        contents.push(loaded_content(store, &target.id).await?);
    }
    contents.sort();
    same(
        "unembedded chunk contents",
        contents,
        vec!["second".to_owned()],
    )?;
    store
        .write(StoredEmbedding {
            target: chunk_target(&only(sweep)?),
            vector: query_vector(),
            content_hash: "test".to_owned(),
            chunks: Vec::new(),
        })
        .await?;
    assert_eq!(
        store.unembedded(EmbedTable::MessageChunks, 10).await?,
        Vec::new()
    );
    assert_eq!(store.unembedded_chunks(kept).await?, Vec::new());
    Ok(())
}

pub(super) fn decreasing_scores(
    results: &[kernel::memory::MemorySearchResult],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut previous = f64::INFINITY;
    for hit in results {
        if hit.relevance_score.total_cmp(&previous).is_ge() {
            return Err("scores are not strictly decreasing".into());
        }
        previous = hit.relevance_score;
    }
    Ok(())
}

fn hit<'a>(
    results: &'a [kernel::memory::MemorySearchResult],
    content: &str,
) -> Result<&'a kernel::memory::MemorySearchResult, Box<dyn std::error::Error>> {
    results
        .iter()
        .find(|row| row.content == content)
        .ok_or_else(|| format!("missing {content}").into())
}

fn span() -> MemoryChunkMatch {
    MemoryChunkMatch {
        index: 0,
        total: 1,
        char_start: 0,
        char_end: 16,
        content: CHUNK_TEXT.to_owned(),
    }
}

fn vector_query(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.to_owned(),
        mode: SearchMode::Vector,
        scope: MemoryScope::Messages,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: kernel::memory::Limit::clamped(Some(10), 10, 1..=50),
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
    }
}

async fn write_parent<S: EmbeddingStore>(store: &S, id: Uuid, vector: Vec<f32>) -> Outcome {
    store
        .write(StoredEmbedding {
            target: EmbedTarget {
                table: EmbedTable::Messages,
                id: id.to_string(),
            },
            vector,
            content_hash: "test".to_owned(),
            chunks: Vec::new(),
        })
        .await?;
    Ok(())
}

async fn write_span<S: MemoryJobStore>(store: &S, id: Uuid, vector: &[f32]) -> Outcome {
    store
        .write_chunks(id, vec![unit_span_vector(0, CHUNK_TEXT, vector)])
        .await?;
    Ok(())
}

fn unit_span(index: u32, content: &str) -> StoredChunk {
    unit_span_vector(index, content, &query_vector())
}

pub(super) fn unit_span_vector(index: u32, content: &str, vector: &[f32]) -> StoredChunk {
    StoredChunk {
        index,
        char_start: 0,
        char_end: 16,
        content: content.to_owned(),
        vector: Some(vector.to_vec()),
    }
}

fn open_span(index: u32, content: &str) -> StoredChunk {
    StoredChunk {
        index,
        char_start: 0,
        char_end: 1,
        content: content.to_owned(),
        vector: None,
    }
}

fn chunk_target(id: &str) -> EmbedTarget {
    EmbedTarget {
        table: EmbedTable::MessageChunks,
        id: id.to_owned(),
    }
}

async fn loaded_content<S: EmbeddingStore>(
    store: &S,
    id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let loaded = store
        .load(&chunk_target(id))
        .await?
        .ok_or("the chunk is missing")?;
    Ok(loaded.content)
}

fn only(targets: Vec<EmbedTarget>) -> Result<String, Box<dyn std::error::Error>> {
    let mut ids = targets.into_iter();
    let id = ids.next().ok_or("no chunk target")?;
    if ids.next().is_some() {
        return Err("more than one chunk target".into());
    }
    Ok(id.id)
}

struct Fixture {
    content: String,
    tool_result: Option<String>,
    mark: Mark,
}

enum Mark {
    Open,
    Chunked,
    Unembeddable,
    Failed,
}

struct Case {
    chars: u32,
    cap: u32,
    limit: u32,
    minutes: &'static [u16],
}

fn fixtures() -> Vec<Fixture> {
    let wide = "\u{e9}".repeat(15);
    let wider = "\u{e9}".repeat(21);
    vec![
        row("a".repeat(21), None, Mark::Chunked),
        row("a".repeat(20), None, Mark::Open),
        row(wide.clone(), None, Mark::Open),
        row(wider.clone(), None, Mark::Open),
        row(format!("   {wide}   \n\t"), None, Mark::Open),
        row(" ".repeat(64), None, Mark::Open),
        row(String::new(), Some(wide), Mark::Open),
        row(String::new(), Some(wider), Mark::Open),
        row("b".repeat(10), Some("c".repeat(9)), Mark::Open),
        row("b".repeat(10), Some("c".repeat(10)), Mark::Open),
        row("\u{e9}".repeat(15), Some(" ".repeat(40)), Mark::Open),
        row("x".repeat(19), Some("\u{1f600}".repeat(3)), Mark::Open),
        row("U".repeat(30), None, Mark::Unembeddable),
        row("F".repeat(30), None, Mark::Failed),
        row(
            " \u{b}\u{c}\r\n".repeat(10),
            Some("d".repeat(30)),
            Mark::Open,
        ),
    ]
}

fn row(content: String, tool_result: Option<String>, mark: Mark) -> Fixture {
    Fixture {
        content,
        tool_result,
        mark,
    }
}

type Trimmed<'a> = (&'a str, Option<&'a str>);

fn cases() -> [Case; 6] {
    [
        Case {
            chars: 20,
            cap: 32_768,
            limit: 100,
            minutes: &[15, 14, 12, 10, 8, 4],
        },
        Case {
            chars: 20,
            cap: 15,
            limit: 100,
            minutes: &[14, 12, 10, 4],
        },
        Case {
            chars: 20,
            cap: 4,
            limit: 100,
            minutes: &[14, 12, 4],
        },
        Case {
            chars: 5,
            cap: 2,
            limit: 100,
            minutes: &[14, 12, 11, 10, 9, 5, 4, 3, 2],
        },
        Case {
            chars: 20,
            cap: 32_768,
            limit: 3,
            minutes: &[15, 14, 12],
        },
        Case {
            chars: 0,
            cap: 0,
            limit: 100,
            minutes: &[14, 12, 11, 10, 9, 5, 4, 3, 2],
        },
    ]
}

fn expected<'a>(
    fixtures: &'a [Fixture],
    minutes: &[u16],
) -> Result<Vec<Trimmed<'a>>, Box<dyn std::error::Error>> {
    let mut rows = Vec::new();
    for minute in minutes {
        let Some(previous) = minute.checked_sub(1) else {
            return Err("minute".into());
        };
        let Some(fixture) = fixtures.get(usize::from(previous)) else {
            return Err("fixture".into());
        };
        rows.push((fixture.content.as_str(), fixture.tool_result.as_deref()));
    }
    Ok(rows)
}

async fn capture_fixtures<S: MemoryStore>(
    store: &S,
    fixtures: &[Fixture],
) -> Result<Vec<Uuid>, Box<dyn std::error::Error>> {
    let base = wire::time::parse_rfc3339("2026-01-01T00:00:00Z")?;
    let mut messages = Vec::new();
    for (index, fixture) in fixtures.iter().enumerate() {
        let at = base.checked_add(SignedDuration::from_secs(i64::try_from(index)? * 60))?;
        messages.push(CaptureMessage {
            event_id: Uuid::new_v4().to_string(),
            role: Role::User,
            content: fixture.content.clone(),
            tool_name: fixture.tool_result.as_ref().map(|_| "Bash".to_owned()),
            tool_args: None,
            tool_result: fixture.tool_result.clone(),
            metadata: None,
            created_at: Some(at),
        });
    }
    let owner = crate::owner()?;
    let captured = store
        .capture(
            kernel::memory::CaptureBatchRequest {
                session_key: "unchunked".to_owned(),
                agent: "rivet".to_owned(),
                channel: Some("cli".to_owned()),
                title: None,
                settings: None,
                task_id: None,
                finalize: None,
                created_at: Some(base),
                updated_at: Some(base),
                messages,
            },
            &owner,
            None,
        )
        .await?;
    ids_of(store, Uuid::parse_str(&captured.conversation_id)?, fixtures).await
}

async fn ids_of<S: MemoryStore>(
    store: &S,
    conversation: Uuid,
    fixtures: &[Fixture],
) -> Result<Vec<Uuid>, Box<dyn std::error::Error>> {
    let mut filter = browse(conversation, None);
    filter.order = MemoryBrowseOrder::Asc;
    let rows = store.browse(filter).await?;
    let mut ids = Vec::new();
    for fixture in fixtures {
        let entry = rows
            .iter()
            .find(|row| row.content == fixture.content && row.tool_result == fixture.tool_result)
            .ok_or("a fixture message is missing")?;
        let text = entry.id.clone().ok_or("a fixture message has no id")?;
        ids.push(Uuid::parse_str(&text)?);
    }
    Ok(ids)
}

async fn mark_fixtures<S>(store: &S, fixtures: &[Fixture], ids: &[Uuid]) -> Outcome
where
    S: MemoryJobStore + EmbeddingStore,
{
    for (fixture, id) in fixtures.iter().zip(ids) {
        let target = EmbedTarget {
            table: EmbedTable::Messages,
            id: id.to_string(),
        };
        match fixture.mark {
            Mark::Open => {}
            Mark::Chunked => {
                store.write_chunks(*id, vec![open_span(0, "x")]).await?;
            }
            Mark::Unembeddable => {
                store
                    .mark_unembeddable(&target, Unembeddable::MediaMarker)
                    .await?;
            }
            Mark::Failed => {
                store.mark_permanent(&target, "failed").await?;
            }
        }
    }
    Ok(())
}
