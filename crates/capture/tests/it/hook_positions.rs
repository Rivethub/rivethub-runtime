use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use capture::hook::ClaudeHook;
use capture::hook::store::{StoreHarness, StoreHook};
use capture::listing::StoreRoots;
use capture::spool::CaptureSpool;
use capture::writer::{CaptureTransport, CaptureWriter, PostError};
use chrono::{DateTime, TimeDelta, Utc};
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::summary_order::{MessagePosition, order_messages};
use parking_lot::Mutex;
use testkit::Json;
use wire::capture::{CaptureBatchRequest, CaptureMessage, CaptureResult};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SESSION: &str = "41414141-4141-4141-4141-414141414141";
const CALL: &str = "toolu_synthetic_read";
const ASK: &str = r#"{"type":"user","uuid":"ask","timestamp":"2026-10-10T12:01:00Z","message":{"role":"user","content":"Find the cabin key"}}"#;
const TURN: &str = r#"{"type":"assistant","uuid":"answer","timestamp":"2026-10-10T12:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"Looking in the drawer"},{"type":"tool_use","id":"toolu_synthetic_read","name":"Read","input":{"path":"drawer"}}]}}"#;
const RESULT: &str = r#"{"type":"user","uuid":"output","timestamp":"2026-10-10T12:00:01Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_synthetic_read","content":"Found it"}]}}"#;
const REPLY: &str = r#"{"type":"assistant","uuid":"reply","timestamp":"2026-10-10T11:59:00Z","message":{"role":"assistant","content":"The key is in the drawer"}}"#;

#[derive(Clone, Default)]
struct Posts(Arc<Mutex<Vec<CaptureMessage>>>);

#[async_trait]
impl CaptureTransport for Posts {
    async fn post(&self, body: String) -> Result<CaptureResult, PostError> {
        let batch: CaptureBatchRequest =
            serde_json::from_str(&body).map_err(|error| PostError::Network {
                message: error.to_string(),
            })?;
        self.0.lock().extend(batch.messages);
        Ok(CaptureResult {
            ok: true,
            conversation_id: "synthetic".to_owned(),
            inserted: 1,
            skipped: 0,
        })
    }
}

fn paths(home: &Path) -> Result<config::Paths, config::ConfigError> {
    let env = config::Environment::from_vars([("HOME".to_owned(), home.display().to_string())]);
    config::Paths::new(&env)
}

fn writer(home: &Path, posts: &Posts) -> CaptureWriter<Posts> {
    CaptureWriter::new(
        posts.clone(),
        CaptureSpool::new(home.join("spool")),
        None,
        None,
    )
}

fn hook(home: &Path, posts: &Posts) -> Result<ClaudeHook<Posts>, config::ConfigError> {
    Ok(ClaudeHook::new(writer(home, posts), &paths(home)?))
}

fn tool(path: &Path, id: Option<&str>) -> Result<String, serde_json::Error> {
    let path = serde_json::to_string(path)?;
    let id = serde_json::to_string(&id)?;
    Ok(format!(
        r#"{{"session_id":"{SESSION}","hook_event_name":"PostToolUse","transcript_path":{path},"tool_use_id":{id},"tool_name":"Read","tool_input":{{"path":"drawer"}},"tool_response":"Found it"}}"#
    ))
}

fn stop(path: &Path) -> Result<String, serde_json::Error> {
    let path = serde_json::to_string(path)?;
    Ok(format!(
        r#"{{"session_id":"{SESSION}","hook_event_name":"Stop","transcript_path":{path}}}"#
    ))
}

fn prompt(content: &str) -> Result<String, serde_json::Error> {
    let prompt = serde_json::to_string(content)?;
    Ok(format!(
        r#"{{"session_id":"{SESSION}","hook_event_name":"UserPromptSubmit","prompt":{prompt}}}"#
    ))
}

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH + TimeDelta::seconds(seconds)
}

fn meta<'a>(message: &'a CaptureMessage, key: &str) -> &'a Json {
    message
        .metadata
        .as_ref()
        .and_then(|data| data.get(key))
        .unwrap_or(&Json::Null)
}

fn ordered(mut messages: Vec<CaptureMessage>, captured_at: DateTime<Utc>) -> Vec<String> {
    messages.sort_by_key(|message| {
        (
            message.created_at.unwrap_or(captured_at),
            message.event_id.clone(),
        )
    });
    order_messages(messages, MemoryBrowseOrder::Asc, |message| {
        MessagePosition::new(
            message.created_at.unwrap_or(captured_at),
            message.role,
            message.metadata.as_ref(),
        )
    })
    .into_iter()
    .map(|message| message.content)
    .collect()
}

#[tokio::test]
async fn a_live_tool_keeps_its_place_even_when_the_positioned_replay_is_deduplicated() -> TestResult
{
    let home = tempfile::tempdir()?;
    let file = home.path().join(format!("{SESSION}.jsonl"));
    std::fs::write(&file, [ASK, "", TURN].join("\n") + "\n")?;
    let posts = Posts::default();
    let capture = hook(home.path(), &posts)?;
    assert_eq!(
        capture
            .run(&tool(&file, Some(CALL))?, at(1), "live")
            .await
            .stderr,
        ""
    );
    std::fs::write(&file, [ASK, "", TURN, RESULT, REPLY].join("\n") + "\n")?;
    assert_eq!(capture.run(&stop(&file)?, at(2), "stop").await.stderr, "");

    let captured_at = DateTime::parse_from_rfc3339("2026-10-10T12:05:00Z")?.with_timezone(&Utc);
    let rows = std::mem::take(&mut *posts.0.lock());
    let live = rows.first().ok_or("live tool post")?;
    assert_eq!(meta(live, "session_jsonl_line"), &2);
    assert_eq!(
        meta(live, "session_jsonl_path"),
        file.to_str().ok_or("fixture path")?
    );
    for row in rows.iter().filter(|row| row.event_id.ends_with(CALL)) {
        assert_eq!(meta(row, "tool_use_id"), CALL);
        assert_eq!(meta(row, "assistant_uuid"), "answer");
    }
    assert_eq!(
        rows.iter()
            .filter(|row| row.event_id == live.event_id)
            .count(),
        2
    );
    let mut seen = HashSet::new();
    let first_writes = rows
        .iter()
        .filter(|row| seen.insert(&row.event_id))
        .cloned()
        .collect();
    assert_eq!(
        ordered(first_writes, captured_at),
        [
            "Find the cabin key",
            "Looking in the drawer",
            "[tool call] Read",
            "The key is in the drawer"
        ]
    );
    Ok(())
}

#[tokio::test]
async fn unanchored_hooks_keep_producer_order_across_restarts_and_retries() -> TestResult {
    let home = tempfile::tempdir()?;
    let file = home.path().join(format!("{SESSION}.jsonl"));
    std::fs::write(&file, [TURN, TURN].join("\n") + "\n")?;
    let posts = Posts::default();
    let capture = hook(home.path(), &posts)?;
    assert_eq!(
        capture
            .run(&prompt("First observation")?, at(4), "first")
            .await
            .stderr,
        ""
    );
    assert_eq!(
        capture
            .run(&tool(&file, Some(CALL))?, at(3), "tool")
            .await
            .stderr,
        ""
    );
    assert_eq!(
        capture
            .run(&tool(&file, None)?, at(2), "idless")
            .await
            .stderr,
        ""
    );
    let restarted = hook(home.path(), &posts)?;
    assert_eq!(
        restarted
            .run(&tool(&file, Some(CALL))?, at(1), "retry")
            .await
            .stderr,
        ""
    );
    assert_eq!(
        restarted
            .run(&prompt("Last observation")?, at(0), "last")
            .await
            .stderr,
        ""
    );
    std::fs::write(&file, "")?;
    assert_eq!(restarted.run(&stop(&file)?, at(5), "stop").await.stderr, "");

    let rows = std::mem::take(&mut *posts.0.lock());
    let [named, idless, retry, last, first] = rows.as_slice() else {
        return Err("expected three tool posts and two prompts".into());
    };
    assert_eq!(meta(named, "tool_use_id"), CALL);
    assert_eq!(meta(named, "session_jsonl_line"), &Json::Null);
    assert_eq!(meta(named, "assistant_uuid"), &Json::Null);
    assert_eq!(meta(idless, "tool_use_id"), &Json::Null);
    assert_eq!(named.event_id, retry.event_id);
    assert_eq!(meta(named, "hook_sequence"), meta(retry, "hook_sequence"));
    let producer = meta(first, "hook_producer")
        .as_str()
        .ok_or("hook producer")?;
    for (row, seq) in [(first, 1), (named, 2), (idless, 3), (last, 4)] {
        assert_eq!(meta(row, "hook_producer"), producer);
        assert_eq!(meta(row, "hook_sequence"), &seq);
    }
    assert_eq!(
        ordered(
            vec![last.clone(), named.clone(), idless.clone(), first.clone()],
            at(5),
        ),
        [
            "First observation",
            "[tool call] Read",
            "[tool call] Read",
            "Last observation"
        ]
    );
    Ok(())
}

#[tokio::test]
async fn grok_hook_posts_the_updates_coordinate_and_native_call_id() -> TestResult {
    let home = tempfile::tempdir()?;
    let root = home.path().join("grok");
    let dir = root.join("sessions/work").join(SESSION);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("updates.jsonl");
    std::fs::write(
        &file,
        concat!(
            "{\"params\":{\"_meta\":{\"agentTimestampMs\":2000},\"update\":{\"sessionUpdate\":\"user_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"Find it\"}}}}\n",
            "{\"params\":{\"_meta\":{\"agentTimestampMs\":1500},\"update\":{\"sessionUpdate\":\"tool_call\",\"toolCallId\":\"call-a\",\"title\":\"Read\"}}}\n",
            "{\"params\":{\"_meta\":{\"agentTimestampMs\":1000},\"update\":{\"sessionUpdate\":\"tool_call_update\",\"toolCallId\":\"call-a\",\"status\":\"completed\"}}}\n"
        ),
    )?;
    let roots = StoreRoots {
        grok: Some(root),
        ..StoreRoots::default()
    };
    let posts = Posts::default();
    let capture = StoreHook::new(
        StoreHarness::GrokBuild,
        writer(home.path(), &posts),
        &roots,
        &paths(home.path())?,
    )
    .ok_or("Grok store")?;
    let payload = format!(r#"{{"session_id":"{SESSION}","hook_event_name":"Stop"}}"#);
    assert_eq!(capture.run(&payload, at(1)).await.stderr, "");
    let rows = std::mem::take(&mut *posts.0.lock());
    let [_, tool] = rows.as_slice() else {
        return Err("expected a prompt and tool post".into());
    };
    assert_eq!(meta(tool, "session_jsonl_line"), &3);
    assert_eq!(
        meta(tool, "session_jsonl_path"),
        file.to_str().ok_or("fixture path")?
    );
    assert_eq!(meta(tool, "tool_use_id"), "call-a");
    assert_eq!(ordered(rows, at(5)), ["Find it", "[tool] Read"]);
    Ok(())
}

#[tokio::test]
async fn metadata_elision_keeps_tool_coordinates_and_correlation() -> TestResult {
    let home = tempfile::tempdir()?;
    let file = home.path().join(format!("{SESSION}.jsonl"));
    std::fs::write(&file, [TURN, RESULT].join("\n") + "\n")?;
    let posts = Posts::default();
    let writer = CaptureWriter::new(
        posts.clone(),
        CaptureSpool::new(home.path().join("spool")),
        std::num::NonZeroUsize::new(2_000),
        None,
    );
    let capture = ClaudeHook::new(writer, &paths(home.path())?);
    let run = capture
        .run(&tool(&file, Some(CALL))?, at(1), &"k".repeat(4_000))
        .await;
    assert_eq!(run.stderr, "");
    let rows = std::mem::take(&mut *posts.0.lock());
    let [row] = rows.as_slice() else {
        return Err("expected one tool post".into());
    };
    assert_eq!(meta(row, "metadata_elided"), &true);
    assert_eq!(
        meta(row, "session_jsonl_path"),
        file.to_str().ok_or("fixture path")?
    );
    assert_eq!(meta(row, "session_jsonl_line"), &1);
    assert_eq!(meta(row, "ordinal"), &1);
    assert_eq!(meta(row, "assistant_uuid"), "answer");
    assert_eq!(meta(row, "tool_use_id"), CALL);
    Ok(())
}

#[tokio::test]
async fn stop_supplies_the_position_of_a_previously_unanchored_tool() -> TestResult {
    let home = tempfile::tempdir()?;
    let file = home.path().join(format!("{SESSION}.jsonl"));
    std::fs::write(&file, format!("{ASK}\n"))?;
    let posts = Posts::default();
    let capture = hook(home.path(), &posts)?;
    let posted_at = DateTime::parse_from_rfc3339("2026-10-10T12:05:00Z")?.with_timezone(&Utc);
    assert_eq!(
        capture
            .run(&tool(&file, Some(CALL))?, posted_at, "live")
            .await
            .stderr,
        ""
    );
    std::fs::write(&file, [ASK, "", TURN, RESULT, REPLY].join("\n") + "\n")?;
    assert_eq!(
        capture.run(&stop(&file)?, posted_at, "stop").await.stderr,
        ""
    );

    let rows = std::mem::take(&mut *posts.0.lock());
    let live = rows.first().ok_or("live tool post")?;
    assert_eq!(meta(live, "session_jsonl_line"), &Json::Null);
    assert_eq!(meta(live, "tool_use_id"), CALL);
    assert!(meta(live, "hook_sequence").as_u64().is_some());
    let replay = rows
        .iter()
        .filter(|row| row.event_id == live.event_id)
        .nth(1)
        .ok_or("transcript replay of the same tool")?;
    assert!(
        meta(replay, "session_jsonl_line").as_u64().is_some(),
        "the later copy carries the transcript line"
    );
    let mut kept = live.clone();
    wire::capture::add_transcript_position(
        kept.metadata.get_or_insert_default(),
        replay.metadata.as_ref(),
    )?;
    let event_id = kept.event_id.clone();
    let retained = std::iter::once(kept)
        .chain(rows.into_iter().filter(|row| row.event_id != event_id))
        .collect();
    assert_eq!(
        ordered(retained, posted_at),
        [
            "Find the cabin key",
            "Looking in the drawer",
            "[tool call] Read",
            "The key is in the drawer"
        ],
        "the kept hook row should sort where the dropped transcript copy would have"
    );
    Ok(())
}

#[tokio::test]
async fn metadata_elision_keeps_the_unanchored_tool_sequence() -> TestResult {
    let home = tempfile::tempdir()?;
    let file = home.path().join(format!("{SESSION}.jsonl"));
    std::fs::write(&file, ASK)?;
    let posts = Posts::default();
    let writer = CaptureWriter::new(
        posts.clone(),
        CaptureSpool::new(home.path().join("spool")),
        std::num::NonZeroUsize::new(2_000),
        None,
    );
    let capture = ClaudeHook::new(writer, &paths(home.path())?);
    let run = capture
        .run(&tool(&file, Some(CALL))?, at(1), &"k".repeat(4_000))
        .await;
    assert_eq!(run.stderr, "");
    let rows = std::mem::take(&mut *posts.0.lock());
    let [row] = rows.as_slice() else {
        return Err("expected one tool post".into());
    };
    assert_eq!(meta(row, "metadata_elided"), &true);
    assert_eq!(meta(row, "session_jsonl_line"), &Json::Null);
    assert_eq!(meta(row, "hook_sequence"), &1);
    let producer = meta(row, "hook_producer").as_str().ok_or("hook producer")?;
    uuid::Uuid::parse_str(producer)?;
    Ok(())
}

#[tokio::test]
async fn unreadable_sequence_files_recover_without_losing_prompts() -> TestResult {
    for bad_ledger in [r#"{"next":1}"#, r#"{"producer":"#] {
        let home = tempfile::tempdir()?;
        let file = home.path().join(format!("{SESSION}.jsonl"));
        std::fs::write(&file, "")?;
        let posts = Posts::default();
        let capture = hook(home.path(), &posts)?;
        assert_eq!(
            capture
                .run(&tool(&file, Some(CALL))?, at(1), "first")
                .await
                .stderr,
            ""
        );
        let ledger = home
            .path()
            .join(format!(".rivethub/claude-pending/{SESSION}.sequence"));
        std::fs::write(&ledger, bad_ledger)?;
        assert_eq!(
            capture
                .run(&prompt("Held observation")?, at(2), "ask")
                .await
                .stderr,
            ""
        );
        assert_eq!(
            capture
                .run(&tool(&file, None)?, at(3), "second")
                .await
                .stderr,
            ""
        );
        assert_eq!(capture.run(&stop(&file)?, at(4), "stop").await.stderr, "");
        let rows = std::mem::take(&mut *posts.0.lock());
        let [first, second, held] = rows.as_slice() else {
            return Err("expected two tools and a held prompt".into());
        };
        assert_eq!(held.content, "Held observation");
        assert_eq!(meta(first, "hook_sequence"), &1);
        assert_eq!(meta(held, "hook_sequence"), &1);
        assert_eq!(meta(second, "hook_sequence"), &2);
        let producer = meta(held, "hook_producer").as_str().ok_or("new producer")?;
        assert_ne!(meta(first, "hook_producer"), producer);
        assert_eq!(meta(second, "hook_producer"), producer);
        let state: Json = serde_json::from_slice(&std::fs::read(&ledger)?)?;
        assert_eq!(state.get("producer"), Some(&Json::from(producer)));
    }
    Ok(())
}

#[tokio::test]
async fn a_failed_sequence_write_still_saves_the_tool_and_pending_prompt() -> TestResult {
    let home = tempfile::tempdir()?;
    let file = home.path().join(format!("{SESSION}.jsonl"));
    std::fs::write(&file, "")?;
    let ledger = home
        .path()
        .join(format!(".rivethub/claude-pending/{SESSION}.sequence"));
    std::fs::create_dir_all(&ledger)?;
    let posts = Posts::default();
    let capture = hook(home.path(), &posts)?;
    assert_eq!(
        capture
            .run(&tool(&file, Some(CALL))?, at(1), "tool")
            .await
            .stderr,
        ""
    );
    assert_eq!(
        capture
            .run(&prompt("Held without a position")?, at(2), "ask")
            .await
            .stderr,
        ""
    );
    assert_eq!(posts.0.lock().len(), 1);
    assert_eq!(capture.run(&stop(&file)?, at(3), "stop").await.stderr, "");
    let rows = std::mem::take(&mut *posts.0.lock());
    let [tool, held] = rows.as_slice() else {
        return Err("expected a tool and a held prompt".into());
    };
    assert_eq!(tool.content, "[tool call] Read");
    assert_eq!(held.content, "Held without a position");
    for row in &rows {
        assert_eq!(meta(row, "hook_producer"), &Json::Null);
        assert_eq!(meta(row, "hook_sequence"), &Json::Null);
    }
    assert!(ledger.is_dir());
    Ok(())
}

#[tokio::test]
async fn opencode_v2_hook_positions_parts_by_native_event_and_part_order() -> TestResult {
    const NATIVE: &str = "ses_synthetic_hook_positions";
    let home = tempfile::tempdir()?;
    let file = home.path().join("opencode.db");
    let db = rusqlite::Connection::open(&file)?;
    db.execute_batch(
        "CREATE TABLE session_v2 (
            id TEXT, title TEXT, model TEXT, parent_id TEXT, time_created INTEGER, time_updated INTEGER
         );
         CREATE TABLE session_message (
            id TEXT, session_id TEXT, seq INTEGER, type TEXT, time_created INTEGER, data TEXT
         );
         CREATE TABLE session_part (
            message_id TEXT, ordinal INTEGER, call_id TEXT, data TEXT
         );",
    )?;
    db.execute(
        "INSERT INTO session_v2 VALUES (?1, 'Cabin inventory', NULL, NULL, 1000, 2000)",
        [NATIVE],
    )?;
    db.execute(
        r#"INSERT INTO session_message VALUES
          ('user', ?1, 1, 'user', 2000, '{"text":"Find it"}'),
          ('assistant', ?1, 2, 'assistant', 1000, '{"time":{"completed":1500},"content":[]}')"#,
        [NATIVE],
    )?;
    db.execute(
        r#"INSERT INTO session_part VALUES
          ('assistant', 1, 'read-a', '{"type":"tool","name":"read","state":{"status":"completed","input":{},"content":[{"type":"text","text":"key"}]}}'),
          ('assistant', 2, NULL, '{"type":"text","text":"Found it"}')"#,
        [],
    )?;
    let roots = StoreRoots {
        opencode: Some(home.path().to_owned()),
        ..StoreRoots::default()
    };
    let posts = Posts::default();
    let capture = StoreHook::new(
        StoreHarness::Opencode,
        writer(home.path(), &posts),
        &roots,
        &paths(home.path())?,
    )
    .ok_or("OpenCode store")?;
    let payload = format!(r#"{{"session_id":"{NATIVE}","hook_event_name":"session.idle"}}"#);
    assert_eq!(capture.run(&payload, at(1)).await.stderr, "");
    let rows = std::mem::take(&mut *posts.0.lock());
    let [_, tool, reply] = rows.as_slice() else {
        return Err("expected a prompt, tool and reply".into());
    };
    assert_eq!(
        meta(tool, "session_sqlite_path"),
        file.to_str().ok_or("fixture path")?
    );
    assert_eq!(meta(tool, "session_store_position"), &131_073);
    assert_eq!(meta(reply, "session_store_position"), &131_074);
    assert_eq!(meta(tool, "tool_use_id"), "read-a");
    assert_eq!(ordered(rows, at(5)), ["Find it", "key", "Found it"]);
    Ok(())
}
