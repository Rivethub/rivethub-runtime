use crate::common;

use common::TestResult;
use common::fresh_store;
use common::instant;
use kernel::memory::NewSummary;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryStore;
use kernel::memory::SummaryWrite;
use kernel::memory::summary_conversion::{ConversionControl, SummaryConversion};
use memory_sqlite::SqliteStore;
use rusqlite::OptionalExtension as _;
use rusqlite::params;
use uuid::Uuid;

const FIRST_LEAF: &str =
    "Investigated the billing timeout on the payment host. Root cause was pool exhaustion.";
const SECOND_LEAF: &str = "A second leaf about weekend gardening plans.";

#[tokio::test]
async fn a_leaf_without_an_opening_never_sets_a_title() -> TestResult {
    let fresh = fresh_store()?;
    let conversation = Uuid::new_v4();
    seed_conversation(&fresh.store, conversation, None).await?;
    let first = fresh
        .store
        .write_summary(leaf(conversation, FIRST_LEAF, "2026-10-10T12:00:00Z")?)
        .await?;
    assert_eq!(first, SummaryWrite::FirstLeaf);
    assert_eq!(title(&fresh.store, conversation).await?, None);
    assert_eq!(fresh.store.backfill_titles(false).await?, 0);
    Ok(())
}

#[tokio::test]
async fn a_harness_title_is_never_replaced_by_a_summary() -> TestResult {
    let fresh = fresh_store()?;
    let conversation = Uuid::new_v4();
    seed_conversation(&fresh.store, conversation, Some("Named by the hook")).await?;
    fresh
        .store
        .write_summary(leaf(conversation, FIRST_LEAF, "2026-10-10T12:00:00Z")?)
        .await?;
    assert_eq!(
        title(&fresh.store, conversation).await?.as_deref(),
        Some("Named by the hook")
    );
    Ok(())
}

#[tokio::test]
async fn the_backfill_titles_from_the_opening() -> TestResult {
    let fresh = fresh_store()?;
    let untitled = Uuid::new_v4();
    let named = Uuid::new_v4();
    let bare = Uuid::new_v4();
    let placeholder = Uuid::new_v4();
    seed_conversation(&fresh.store, untitled, None).await?;
    seed_conversation(&fresh.store, named, Some("Kept")).await?;
    seed_conversation(&fresh.store, bare, None).await?;
    seed_conversation(&fresh.store, placeholder, Some("Grok Build session")).await?;
    seed_opening(
        &fresh.store,
        placeholder,
        "**Session metadata**\n- Two messages.\n\n**Billing timeout fix**\n- Fixed.",
    )
    .await?;
    seed_opening(&fresh.store, untitled, FIRST_LEAF).await?;
    seed_opening(&fresh.store, named, FIRST_LEAF).await?;
    fresh
        .store
        .write_summary(leaf(untitled, SECOND_LEAF, "2026-10-10T12:00:00Z")?)
        .await?;
    assert_eq!(fresh.store.backfill_titles(true).await?, 2);
    assert_eq!(title(&fresh.store, untitled).await?, None);
    assert_eq!(fresh.store.backfill_titles(false).await?, 2);
    assert_eq!(
        title(&fresh.store, untitled).await?.as_deref(),
        Some("Investigated the billing timeout on the payment host.")
    );
    assert_eq!(
        title(&fresh.store, placeholder).await?.as_deref(),
        Some("Billing timeout fix")
    );
    assert_eq!(title(&fresh.store, named).await?.as_deref(), Some("Kept"));
    assert_eq!(title(&fresh.store, bare).await?, None);
    assert_eq!(fresh.store.backfill_titles(false).await?, 0);
    Ok(())
}

fn leaf(conversation: Uuid, content: &str, at: &str) -> TestResult<NewSummary> {
    let at = instant(at)?;
    Ok(NewSummary {
        id: Uuid::new_v4(),
        conversation_id: conversation,
        kind: SummaryKind::Leaf,
        depth: SummaryKind::Leaf.depth(),
        content: content.to_owned(),
        message_count: 0,
        earliest_at: at,
        latest_at: at,
        model: "fake-chat".to_owned(),
        pipeline_version: 5,
        created_at: at,
        sources: Vec::new(),
        child_ids: Vec::new(),
    })
}

async fn seed_conversation(
    store: &SqliteStore,
    id: Uuid,
    title: Option<&'static str>,
) -> TestResult {
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_conversations \
                    (id, session_key, agent, channel, title, settings, active, created_at, updated_at) \
                 VALUES (?1, ?1, 'rivet', 'cli', ?2, '{}', 1, ?3, ?3)",
                params![id.to_string(), title, common::RECORDED_START],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

async fn seed_opening(store: &SqliteStore, id: Uuid, content: &str) -> TestResult {
    let content = content.to_owned();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO rh_summary_openings (conversation_id, content) VALUES (?1, ?2)",
                params![id.to_string(), content],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

async fn title(store: &SqliteStore, id: Uuid) -> TestResult<Option<String>> {
    let found = store
        .with_conn(move |conn| {
            conn.query_row(
                "SELECT title FROM ros_conversations WHERE id = ?1",
                params![id.to_string()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(common::seed)
        })
        .await?;
    Ok(found.ok_or("conversation")?)
}

#[tokio::test]
async fn capture_keeps_one_opening_and_reorders_it_when_the_start_arrives() -> TestResult {
    use kernel::filing::FilingStore as _;

    let fresh = fresh_store()?;
    let owner = testkit::owner()?;
    let now = instant("2026-10-10T12:00:00Z")?;
    let later = batch(7, "Investigate the retry budget")?;
    let result = fresh
        .store
        .capture(later.clone(), &owner, None, now)
        .await?;
    let id = Uuid::parse_str(&result.conversation_id)?;
    let before = fresh.store.opening_title(id).await?.expect("opening");
    assert_eq!(before.content, "Investigate the retry budget");
    assert!(before.is_seed());
    assert_eq!(
        fresh.store.undelivered_openings().await?,
        [],
        "capture queued both jobs"
    );

    let earlier = batch(2, "Fix the queue receipt")?;
    fresh
        .store
        .capture(
            earlier,
            &owner,
            None,
            now + jiff::SignedDuration::from_secs(1),
        )
        .await?;
    let duplicate = fresh.store.capture(later, &owner, None, now).await?;
    assert_eq!(duplicate.inserted, 0);
    assert!(
        !fresh
            .store
            .finish_opening_title(&before, Some("Stale model title"))
            .await?
    );
    let after = fresh
        .store
        .opening_title(id)
        .await?
        .expect("earlier opening");
    assert_eq!(after.content, "Fix the queue receipt");
    assert!(after.is_seed());
    assert_eq!(
        fresh.store.subject(id).await?.expect("subject").summary,
        Some(after.content)
    );
    let rows = common::table(&fresh.store, "rh_summary_openings").await?;
    assert_eq!(rows.len(), 1);
    let jobs = common::table(&fresh.store, "ros_jobs").await?;
    for task in ["file-conversation", "title-conversation"] {
        assert_eq!(jobs.iter().filter(|job| job["task"] == task).count(), 1);
    }
    Ok(())
}

fn batch(line: u64, text: &str) -> TestResult<kernel::memory::CaptureBatchRequest> {
    use kernel::memory::{CaptureBatchRequest, CaptureMessage};
    use wire::{JsonObject, message::Role};

    let mut metadata = JsonObject::default();
    metadata.insert("session_jsonl_line", line)?;
    metadata.insert("session_jsonl_path", "/invented/session.jsonl")?;
    Ok(CaptureBatchRequest {
        session_key: "cli:projected".to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("cli".to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![CaptureMessage {
            event_id: format!("line-{line}"),
            role: Role::User,
            content: text.to_owned(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: Some(metadata),
            created_at: None,
        }],
    })
}

#[tokio::test]
async fn legacy_leaves_are_materialized_once_by_the_sqlite_upgrade() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("legacy.sqlite");
    migrate::sqlite(&path)?;
    let id = Uuid::new_v4();
    {
        let conn = rusqlite::Connection::open(&path)?;
        conn.execute_batch(
            "ALTER TABLE ros_conversations DROP COLUMN title_from_capture; PRAGMA user_version=36;",
        )?;
        conn.execute(
            "INSERT INTO ros_conversations (id, session_key, agent, channel, created_at, updated_at)
             VALUES (?1, 'cli:legacy-opening', 'rivet', 'cli', ?2, ?2)",
            params![id.to_string(), common::RECORDED_START],
        )?;
        conn.execute(
            "INSERT INTO ros_summaries (id, conversation_id, kind, depth, content,
                                        message_count, earliest_at, latest_at, created_at)
             VALUES (?1, ?2, 'leaf', 0, ?3, 0, ?4, ?4, ?4)",
            params![
                Uuid::new_v4().to_string(),
                id.to_string(),
                FIRST_LEAF,
                common::RECORDED_START
            ],
        )?;
    }
    migrate::sqlite(&path)?;
    let store = SqliteStore::open(&path)?;
    let opening = store.opening_title(id).await?.expect("historical opening");
    assert_eq!(opening.content, FIRST_LEAF);
    assert!(!opening.armed);
    assert_eq!(
        store.undelivered_openings().await?,
        Vec::<kernel::queue::Job>::new()
    );
    store
        .control(ConversionControl::ArmOpening { conversation: id })
        .await?;
    assert_eq!(store.undelivered_openings().await?.len(), 2);
    assert_eq!(store.backfill_titles(false).await?, 1);
    Ok(())
}
