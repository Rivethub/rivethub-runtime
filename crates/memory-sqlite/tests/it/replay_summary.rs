use crate::common;

use common::Scenario;
use common::ScenarioFile;
use common::TestResult;
use common::compare_tables;
use common::fresh_store;
use common::instant;
use common::recording;
use kernel::memory::NewSummary;
use kernel::memory::SummaryKind;
use kernel::memory::SummarySource;
use kernel::memory::SummaryStore;
use kernel::memory::SummaryStoreError;
use kernel::memory::ToolUse;
use rusqlite::params;
use testkit::Json;
use uuid::Uuid;

const LEAF: &str = "leaf once a full window exists";
const WORKERS: &str = "the summarizer job loop and its chat calls belong to the workers";

const SKIPPED: &[(&str, &str)] = &[
    (LEAF, WORKERS),
    ("leaves → branches → root with small batches", WORKERS),
    ("truncated answer shrinks the leaf batch", WORKERS),
    (
        "failing LLM fails the job; heartbeat never summarized",
        WORKERS,
    ),
    (
        "permanent failure → dead → rests an hour → revived",
        WORKERS,
    ),
    ("below the floor until idle/stale", WORKERS),
];

#[tokio::test]
async fn every_summarizer_scenario_is_replayed_or_skipped() -> TestResult {
    let file: ScenarioFile = recording("summarizer.json")?;
    for scenario in &file.scenarios {
        let skipped = SKIPPED
            .iter()
            .any(|(name, reason)| *name == scenario.name && !reason.is_empty());
        assert!(skipped, "{}", scenario.name);
    }
    for (name, _) in SKIPPED {
        assert!(file.scenarios.iter().any(|scenario| scenario.name == *name));
    }
    Ok(())
}

#[tokio::test]
async fn recorded_leaf_round_trips_through_the_store() -> TestResult {
    let file: ScenarioFile = recording("summarizer.json")?;
    let scenario = file
        .scenarios
        .iter()
        .find(|scenario| scenario.name == LEAF)
        .ok_or("missing leaf scenario")?;
    let fresh = fresh_store()?;
    let summary = recorded_summary(scenario)?;
    seed_messages(&fresh.store, scenario).await?;
    seed_pending(&fresh.store).await?;
    fresh.store.write_summary(summary).await?;
    let conversation = Uuid::parse_str("00000000-0000-4000-8000-000000000002")?;
    let window = fresh
        .store
        .conversation(conversation)
        .await?
        .ok_or("conversation window")?;
    let leaf = window.leaves.first().ok_or("leaf")?;
    assert_eq!(window.messages.len(), 0);
    assert_eq!(window.branches.len(), 0);
    assert_eq!(leaf.kind, SummaryKind::Leaf);
    assert_eq!(leaf.depth, 0);
    assert_eq!(leaf.message_count, 10);
    assert_eq!(leaf.content, recorded_content(scenario)?);
    let pending = fresh.store.pending().await?;
    let open = pending.first().ok_or("pending conversation")?;
    assert_eq!(pending.len(), 1);
    assert_eq!(
        open.id,
        Uuid::parse_str("10000000-0000-4000-8000-000000000001")?
    );
    assert_eq!(open.unsummarized, 1);
    let mut ids = common::Ids::default();
    compare_tables(&fresh.store, &scenario.tables, &mut ids, LEAF).await?;
    Ok(())
}

#[tokio::test]
async fn pending_lists_the_most_recent_activity_first() -> TestResult {
    let fresh = fresh_store()?;
    let seeded = [
        (
            "20000000-0000-4000-8000-000000000001",
            "2026-10-11T09:00:00.000Z",
        ),
        (
            "20000000-0000-4000-8000-000000000002",
            "2026-10-30T17:00:00.000Z",
        ),
        (
            "20000000-0000-4000-8000-000000000003",
            "2026-10-24T12:00:00.000Z",
        ),
    ];
    for (index, (id, latest)) in seeded.iter().enumerate() {
        seed_conversation(&fresh.store, id, &format!("cli:{index}"), "rivet").await?;
        for (step, at) in ["2026-10-01T08:00:00.000Z", latest].iter().enumerate() {
            message(
                &fresh.store,
                &format!("30000000-0000-4000-8000-0000000000{index}{step}"),
                id,
                body("rivet", "user", "a message", at),
            )
            .await?;
        }
    }
    let order: Vec<Uuid> = fresh
        .store
        .pending()
        .await?
        .into_iter()
        .map(|conversation| conversation.id)
        .collect();
    let expected = [
        "20000000-0000-4000-8000-000000000002",
        "20000000-0000-4000-8000-000000000003",
        "20000000-0000-4000-8000-000000000001",
    ]
    .into_iter()
    .map(Uuid::parse_str)
    .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(order, expected);
    Ok(())
}

#[tokio::test]
async fn a_missing_summary_child_is_rejected() -> TestResult {
    let fresh = fresh_store()?;
    let conversation = Uuid::new_v4();
    let summary = Uuid::new_v4();
    seed_conversation(
        &fresh.store,
        &conversation.to_string(),
        "cli:child",
        "rivet",
    )
    .await?;
    let outcome = fresh
        .store
        .write_summary(NewSummary {
            id: summary,
            conversation_id: conversation,
            kind: SummaryKind::Branch,
            depth: 1,
            content: "branch".to_owned(),
            message_count: 0,
            earliest_at: instant(common::RECORDED_START)?,
            latest_at: instant(common::RECORDED_START)?,
            model: "fake-chat".to_owned(),
            pipeline_version: 5,
            created_at: instant(common::RECORDED_START)?,
            sources: Vec::new(),
            child_ids: vec![Uuid::new_v4()],
        })
        .await;
    match outcome {
        Err(SummaryStoreError::Missing { .. }) => Ok(()),
        other => Err(format!("missing child returned {other:?}").into()),
    }
}

fn recorded_summary(scenario: &Scenario) -> TestResult<NewSummary> {
    let row = scenario
        .tables
        .get("ros_summaries")
        .and_then(|rows| rows.first())
        .ok_or("summary row")?;
    let mut sources = Vec::new();
    for source in scenario
        .tables
        .get("ros_summary_sources")
        .ok_or("summary sources")?
    {
        sources.push(SummarySource {
            message_id: Uuid::parse_str(text(source, "message_id")?)?,
            ordinal: number(source, "ordinal")?,
        });
    }
    Ok(NewSummary {
        id: Uuid::parse_str(text(row, "id")?)?,
        conversation_id: Uuid::parse_str(text(row, "conversation_id")?)?,
        kind: SummaryKind::Leaf,
        depth: number(row, "depth")?,
        content: text(row, "content")?.to_owned(),
        message_count: u32::try_from(number(row, "message_count")?)?,
        earliest_at: instant(text(row, "earliest_at")?)?,
        latest_at: instant(text(row, "latest_at")?)?,
        model: text(row, "model")?.to_owned(),
        pipeline_version: number(row, "pipeline_version")?,
        created_at: instant(text(row, "created_at")?)?,
        sources,
        child_ids: Vec::new(),
    })
}

fn recorded_content(scenario: &Scenario) -> TestResult<&str> {
    scenario
        .tables
        .get("ros_summaries")
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("content"))
        .and_then(Json::as_str)
        .ok_or_else(|| "summary content".into())
}

#[tokio::test]
async fn a_tool_row_shows_its_clipped_input_and_result() -> TestResult {
    let fresh = fresh_store()?;
    let conversation = "20000000-0000-4000-8000-000000000001";
    seed_conversation(&fresh.store, conversation, "cli:tools", "rivet").await?;
    message(
        &fresh.store,
        "20000000-0000-4000-8000-000000000002",
        conversation,
        body("rivet", "user", "run it", "2026-10-10T12:00:00.000Z"),
    )
    .await?;
    let args = format!(r#"{{"command":"{}"}}"#, "\u{e9}".repeat(400));
    let output = "line\n".repeat(200);
    let row = (args.clone(), output.clone());
    fresh
        .store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_messages \
                    (id, conversation_id, agent, channel, role, content, tool_name, tool_args, \
                     tool_result, metadata, created_at) \
                 VALUES ('20000000-0000-4000-8000-000000000003', ?1, 'rivet', 'cli', 'tool', \
                         '[tool call] Bash', 'Bash', ?2, ?3, '{}', '2026-10-10T12:01:00.000Z')",
                params![conversation, row.0, row.1],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    let window = fresh
        .store
        .conversation(Uuid::parse_str(conversation)?)
        .await?
        .ok_or("conversation window")?;
    let tools: Vec<Option<ToolUse>> = window
        .messages
        .into_iter()
        .map(|message| message.tool)
        .collect();
    let [None, Some(tool)] = tools.as_slice() else {
        return Err("one tool row after the user message".into());
    };
    let input = tool
        .input
        .strip_suffix(" \u{2026}")
        .ok_or("clipped input")?;
    assert!(input.len() <= ToolUse::INPUT_BYTES && args.starts_with(input));
    let shown = tool
        .result
        .strip_suffix(" \u{2026}")
        .ok_or("clipped result")?;
    assert!(shown.len() <= ToolUse::RESULT_BYTES && output.starts_with(shown));
    Ok(())
}

async fn seed_messages(store: &memory_sqlite::SqliteStore, scenario: &Scenario) -> TestResult {
    let row = scenario
        .tables
        .get("ros_summaries")
        .and_then(|rows| rows.first())
        .ok_or("summary row")?;
    let conversation = text(row, "conversation_id")?;
    seed_conversation(store, conversation, "cli:leaf", "rivet").await?;
    let sources = scenario
        .tables
        .get("ros_summary_sources")
        .ok_or("summary sources")?;
    for (index, source) in sources.iter().enumerate() {
        let at = format!("2026-10-10T12:0{index}:00.000Z");
        message(
            store,
            text(source, "message_id")?,
            conversation,
            body("rivet", "user", &format!("leaf message {index}"), &at),
        )
        .await?;
    }
    Ok(())
}

async fn seed_pending(store: &memory_sqlite::SqliteStore) -> TestResult {
    seed_conversation(
        store,
        "10000000-0000-4000-8000-000000000001",
        "cli:open",
        "rivet",
    )
    .await?;
    message(
        store,
        "10000000-0000-4000-8000-000000000002",
        "10000000-0000-4000-8000-000000000001",
        body("rivet", "user", "still open", "2026-10-10T12:00:00.000Z"),
    )
    .await?;
    seed_conversation(
        store,
        "10000000-0000-4000-8000-000000000003",
        "heartbeat:pulse",
        "rivet",
    )
    .await?;
    message(
        store,
        "10000000-0000-4000-8000-000000000004",
        "10000000-0000-4000-8000-000000000003",
        body("rivet", "system", "pulse", "2026-10-10T12:00:00.000Z"),
    )
    .await?;
    Ok(())
}

async fn seed_conversation(
    store: &memory_sqlite::SqliteStore,
    id: &str,
    session: &str,
    agent: &str,
) -> TestResult {
    let id = id.to_owned();
    let session = session.to_owned();
    let agent = agent.to_owned();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_conversations \
                    (id, session_key, agent, channel, settings, active, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, 'cli', '{}', 1, ?4, ?4)",
                params![id, session, agent, common::RECORDED_START],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

struct MessageRow<'a> {
    agent: &'a str,
    role: &'a str,
    content: &'a str,
    at: &'a str,
}

fn body<'a>(agent: &'a str, role: &'a str, content: &'a str, at: &'a str) -> MessageRow<'a> {
    MessageRow {
        agent,
        role,
        content,
        at,
    }
}

async fn message(
    store: &memory_sqlite::SqliteStore,
    id: &str,
    conversation: &str,
    body: MessageRow<'_>,
) -> TestResult {
    let id = id.to_owned();
    let conversation = conversation.to_owned();
    let agent = body.agent.to_owned();
    let role = body.role.to_owned();
    let content = body.content.to_owned();
    let at = body.at.to_owned();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_messages \
                    (id, conversation_id, agent, channel, role, content, metadata, created_at) \
                 VALUES (?1, ?2, ?3, 'cli', ?4, ?5, '{}', ?6)",
                params![id, conversation, agent, role, content, at],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

fn text<'a>(row: &'a Json, key: &str) -> TestResult<&'a str> {
    row.get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| format!("missing {key}").into())
}

fn number(row: &Json, key: &str) -> TestResult<i32> {
    row.get(key)
        .and_then(Json::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| format!("missing {key}").into())
}
