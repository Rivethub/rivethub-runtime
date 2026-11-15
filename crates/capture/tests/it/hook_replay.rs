use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use parking_lot::Mutex;
use testkit::capture::entries;
use wire::capture::CaptureResult;

#[derive(Clone)]
struct FakeTransport {
    posts: Arc<Mutex<Vec<String>>>,
}

impl FakeTransport {
    fn new() -> Self {
        Self {
            posts: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl capture::writer::CaptureTransport for FakeTransport {
    async fn post(&self, body: String) -> Result<CaptureResult, capture::writer::PostError> {
        let parsed: testkit::Json =
            serde_json::from_str(&body).map_err(|error| capture::writer::PostError::Network {
                message: format!("the hook posted a body that is not JSON: {error}"),
            })?;
        let inserted = parsed
            .get("messages")
            .and_then(testkit::Json::as_array)
            .map(Vec::len)
            .ok_or_else(|| capture::writer::PostError::Network {
                message: "the posted batch body carries no messages array".to_owned(),
            })?;
        self.posts.lock().push(body);
        Ok(CaptureResult {
            ok: true,
            conversation_id: "conv-1".to_owned(),
            inserted: inserted as u64,
            skipped: 0,
        })
    }
}

const NOW_TEXT: &str = "2026-10-10T18:17:23.937Z";

fn hook_in(
    home: &Path,
    transport: &FakeTransport,
) -> Result<capture::hook::ClaudeHook<FakeTransport>, config::ConfigError> {
    let env = config::Environment::from_vars([("HOME".to_owned(), home.display().to_string())]);
    let spool = capture::spool::CaptureSpool::new(home.join("spool"));
    let writer = capture::writer::CaptureWriter::new(
        transport.clone(),
        spool,
        std::num::NonZeroUsize::new(768 * 1024),
        None,
    );
    Ok(capture::hook::ClaudeHook::new(
        writer,
        &config::Paths::new(&env)?,
    ))
}

fn prepare_store(root: &Path) -> std::io::Result<PathBuf> {
    let store = root.join("store").join("projects").join("-work-demo");
    std::fs::create_dir_all(&store)?;
    let target = store.join("33333333-3333-3333-3333-333333333333.jsonl");
    std::fs::copy(
        testkit::recording("capture/claude-hook/transcript.jsonl"),
        &target,
    )?;
    Ok(target)
}

async fn posted_bodies(
    entry: &testkit::capture::EntryRecord,
    transcript_text: &str,
    now: DateTime<Utc>,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let inner = tempfile::tempdir()?;
    let transport = FakeTransport::new();
    let hook = hook_in(inner.path(), &transport)?;
    let stdin = entry.stdin.replace("__TRANSCRIPT__", transcript_text);
    let run = hook.run(&stdin, now, "1791656243919-30e4c5d70").await;
    assert_eq!(run.exit, 0, "{}", run.stderr);
    let posts = transport.posts.lock().clone();
    Ok(posts)
}

#[tokio::test]
async fn malformed_payloads_exit_cleanly() -> Result<(), Box<dyn std::error::Error>> {
    let inner = tempfile::tempdir()?;
    let transport = FakeTransport::new();
    let hook = hook_in(inner.path(), &transport)?;
    let now: DateTime<Utc> = NOW_TEXT.parse()?;
    for stdin in ["{", "{}", "", "{\"hook_event_name\":\"UserPromptSubmit\"}"] {
        let run = hook.run(stdin, now, "1791656243919-30e4c5d70").await;
        assert_eq!(run.exit, 0, "stdin {stdin}");
        assert_eq!(run.stderr, "", "stdin {stdin}");
        assert!(transport.posts.lock().is_empty(), "stdin {stdin}");
    }
    Ok(())
}

#[tokio::test]
async fn an_empty_prompt_is_not_held() -> Result<(), Box<dyn std::error::Error>> {
    let inner = tempfile::tempdir()?;
    let transport = FakeTransport::new();
    let hook = hook_in(inner.path(), &transport)?;
    let now: DateTime<Utc> = NOW_TEXT.parse()?;
    let stdin = r#"{"session_id":"33333333-3333-3333-3333-333333333333","hook_event_name":"UserPromptSubmit","prompt":"  "}"#;
    let run = hook.run(stdin, now, "1791656243919-30e4c5d70").await;
    assert_eq!((run.exit, run.stderr.as_str()), (0, ""));
    assert!(transport.posts.lock().is_empty());
    assert!(!inner.path().join(".rivethub/claude-pending").exists());
    Ok(())
}

#[tokio::test]
async fn a_failed_log_write_does_not_fail_the_run() -> Result<(), Box<dyn std::error::Error>> {
    let inner = tempfile::tempdir()?;
    std::fs::write(inner.path().join(".rivethub"), b"not a directory")?;
    let transport = FakeTransport::new();
    let hook = hook_in(inner.path(), &transport)?;
    let now: DateTime<Utc> = NOW_TEXT.parse()?;
    let stdin = r#"{"session_id":"33333333-3333-3333-3333-333333333333","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"toolu_1"}"#;
    let run = hook.run(stdin, now, "1791656243919-30e4c5d70").await;
    assert_eq!(run.exit, 0);
    assert_eq!(run.stderr, "");
    assert_eq!(transport.posts.lock().len(), 1);
    Ok(())
}

#[tokio::test]
async fn transcript_tool_arguments_preserve_input_values() -> Result<(), Box<dyn std::error::Error>>
{
    let rows = entries(&testkit::recording("capture/claude-hook/hook-ts.json"))?;
    let outer = tempfile::tempdir()?;
    let transcript = prepare_store(outer.path())?;
    let source = std::fs::read_to_string(&transcript)?;
    let parsed = capture::transcript::parse_lines(&source);
    let expected: Vec<_> = parsed
        .lines
        .iter()
        .flat_map(capture::transcript::Line::tool_uses)
        .map(|tool| (tool.name.to_owned(), tool.input.arguments().clone()))
        .collect();
    assert_eq!(expected.len(), 1);
    let arguments = &expected.first().ok_or("missing source tool arguments")?.1;
    assert_eq!(
        arguments.get("big"),
        Some(&testkit::Json::from(12_345_678_901_234_567_890_u64))
    );
    assert_eq!(arguments.get("timeout"), Some(&testkit::Json::from(1.0)));
    for index in [3, 4, 5] {
        let entry = rows
            .iter()
            .find(|entry| entry.index == index)
            .ok_or("missing transcript case")?;
        let posts = posted_bodies(
            entry,
            transcript.to_str().ok_or("non-UTF-8 transcript path")?,
            NOW_TEXT.parse()?,
        )
        .await?;
        let mut actual = Vec::new();
        for body in posts {
            let batch: wire::capture::CaptureBatchRequest = serde_json::from_str(&body)?;
            for message in batch.messages {
                if let Some(arguments) = message.tool_args {
                    actual.push((
                        message.tool_name.ok_or("tool arguments require a name")?,
                        arguments,
                    ));
                }
            }
        }
        assert_eq!(actual, expected, "hook/{index}");
    }
    Ok(())
}

fn store_with_thinking(root: &Path) -> std::io::Result<PathBuf> {
    let transcript = prepare_store(root)?;
    let mut lines = std::fs::read_to_string(&transcript)?;
    lines.push_str(r#"{"type":"assistant","uuid":"a3","timestamp":"2026-10-10T12:00:05.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Counting done.","signature":"x"}]}}"#);
    lines.push('\n');
    std::fs::write(&transcript, lines)?;
    Ok(transcript)
}

#[tokio::test]
async fn a_turn_end_keys_rows_by_transcript_uuid_and_flushes_held_prompts()
-> Result<(), Box<dyn std::error::Error>> {
    let outer = tempfile::tempdir()?;
    let transcript = store_with_thinking(outer.path())?;
    let transport = FakeTransport::new();
    let hook = hook_in(outer.path(), &transport)?;
    let now: DateTime<Utc> = NOW_TEXT.parse()?;
    let session = "33333333-3333-3333-3333-333333333333";
    for (prompt, key) in [
        ("list the files", "k1"),
        ("<task-notification>done</task-notification>", "k2"),
    ] {
        let held = format!(
            r#"{{"session_id":"{session}","hook_event_name":"UserPromptSubmit","prompt":"{prompt}"}}"#
        );
        assert_eq!(hook.run(&held, now, key).await.stderr, "");
    }
    assert!(transport.posts.lock().is_empty());
    let stop = format!(
        r#"{{"session_id":"{session}","hook_event_name":"Stop","transcript_path":"{}"}}"#,
        transcript.display()
    );
    assert_eq!(hook.run(&stop, now, "k3").await.stderr, "");
    let posts = transport.posts.lock().clone();
    let [body] = posts.as_slice() else {
        return Err(format!("expected one post, got {}", posts.len()).into());
    };
    let batch: wire::capture::CaptureBatchRequest = serde_json::from_str(body)?;
    let prefix = format!("claude-code:{session}:");
    let mut ids: Vec<String> = batch
        .messages
        .iter()
        .map(|message| message.event_id.replace(&prefix, ""))
        .map(|id| {
            if id.starts_with("occ:") {
                "occ".to_owned()
            } else {
                id
            }
        })
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        ["a1", "a2", "a3", "hook:k2", "occ", "tool:toolu_1", "u1"]
    );
    let content = |id: &str| {
        batch
            .messages
            .iter()
            .find(|message| message.event_id == format!("{prefix}{id}"))
            .map(|message| (message.content.clone(), message.tool_result.clone()))
    };
    assert_eq!(
        content("a3"),
        Some(("[thinking] Counting done.".to_owned(), None))
    );
    assert_eq!(
        content("tool:toolu_1"),
        Some((
            "[tool call] Bash".to_owned(),
            Some("a.txt\nb.txt".to_owned())
        ))
    );
    assert_eq!(
        content("hook:k2"),
        Some((
            "<task-notification>done</task-notification>".to_owned(),
            None
        ))
    );
    assert!(
        !outer
            .path()
            .join(".rivethub/claude-pending")
            .join(format!("{session}.jsonl"))
            .exists()
    );
    Ok(())
}
