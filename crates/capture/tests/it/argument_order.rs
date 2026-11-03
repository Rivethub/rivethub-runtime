use std::sync::Arc;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use parking_lot::Mutex;
use testkit::Json;
use wire::capture::CaptureResult;

use capture::event_id::OccurrenceKind;
use capture::event_id::ToolInput;
use capture::event_id::occurrence_digest;
use capture::event_id::occurrence_event_id;
use capture::hook::ClaudeHook;
use capture::spool::CaptureSpool;
use capture::transcript::parse_lines;
use capture::writer::CaptureTransport;
use capture::writer::CaptureWriter;
use capture::writer::PostError;

const SESSION: &str = "33333333-3333-3333-3333-333333333333";
const NOW_TEXT: &str = "2026-10-10T18:17:23.937Z";
const TRANSCRIPT_LINE: &str = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-10-10T12:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Running ls."},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"z":1,"a":2}},{"type":"tool_use","id":"toolu_2","name":"Bash","input":{"z":1,"a":2}},{"type":"tool_use","id":"toolu_3","name":"Bash","input":{"a":1}}]}}"#;

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
impl CaptureTransport for FakeTransport {
    async fn post(&self, body: String) -> Result<CaptureResult, PostError> {
        self.posts.lock().push(body);
        Ok(CaptureResult {
            ok: true,
            conversation_id: "conv-1".to_owned(),
            inserted: 1,
            skipped: 0,
        })
    }
}

#[test]
fn transcript_tool_input_keeps_source_order() {
    let parsed = parse_lines(TRANSCRIPT_LINE);
    assert!(parsed.skipped.is_empty(), "{:?}", parsed.skipped);
    let spellings: Vec<String> = parsed
        .lines
        .first()
        .expect("assistant line parses")
        .tool_uses()
        .iter()
        .map(|use_| use_.input.spelling().to_owned())
        .collect();
    assert_eq!(
        spellings,
        [r#"{"z":1,"a":2}"#, r#"{"z":1,"a":2}"#, r#"{"a":1}"#]
    );
}

#[tokio::test]
async fn occurrence_count_spans_hook_and_transcript_spellings()
-> Result<(), Box<dyn std::error::Error>> {
    let inner = tempfile::tempdir()?;
    let transcript_path = inner.path().join(format!("{SESSION}.jsonl"));
    std::fs::write(&transcript_path, format!("{TRANSCRIPT_LINE}\n"))?;

    let transport = FakeTransport::new();
    let spool = CaptureSpool::new(inner.path().join("spool"));
    let writer = CaptureWriter::new(
        transport.clone(),
        spool,
        std::num::NonZeroUsize::new(768 * 1024),
        None,
    );
    let env =
        config::Environment::from_vars([("HOME".to_owned(), inner.path().display().to_string())]);
    let hook = ClaudeHook::new(writer, &config::Paths::new(&env)?);
    let stdin = format!(
        r#"{{"session_id":"{SESSION}","transcript_path":"{}","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{{"z":1,"a":2}}}}"#,
        transcript_path.display()
    );

    let now: DateTime<Utc> = NOW_TEXT.parse()?;
    let run = hook.run(&stdin, now, "1791656243919-30e4c5d70").await;
    assert_eq!(run.exit, 0, "{}", run.stderr);
    assert_eq!(run.stderr, "");

    let posts = transport.posts.lock().clone();
    let body = posts.first().ok_or("expected one post")?;
    let parsed: Json = serde_json::from_str(body)?;
    let message = parsed
        .get("messages")
        .and_then(Json::as_array)
        .and_then(|messages| messages.first())
        .ok_or("expected one message")?;

    let value = ToolInput::parse(r#"{"z":1,"a":2}"#)?;
    let transcript_spelling = {
        let parsed = parse_lines(TRANSCRIPT_LINE);
        let first_use = parsed
            .lines
            .first()
            .expect("assistant line parses")
            .tool_uses()
            .into_iter()
            .next()
            .expect("assistant line carries a tool call");
        first_use.input.spelling().to_owned()
    };
    let hook_spelling = value.spelling();
    assert_eq!(transcript_spelling, hook_spelling);
    assert_eq!(transcript_spelling, r#"{"z":1,"a":2}"#);

    let digest = occurrence_digest(
        OccurrenceKind::Tool,
        "[tool call] Bash",
        "Bash",
        Some(&value),
    );
    let expected = occurrence_event_id(&format!("claude-code:{SESSION}"), &digest, 2);
    assert_eq!(
        message.get("event_id").and_then(Json::as_str),
        Some(expected.as_str())
    );

    let tool_args = message.get("tool_args").ok_or("expected tool_args")?;
    assert_eq!(tool_args.get("z").and_then(Json::as_i64), Some(1));
    assert_eq!(tool_args.get("a").and_then(Json::as_i64), Some(2));
    Ok(())
}
