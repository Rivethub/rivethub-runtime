use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use parking_lot::Mutex;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureResult;
use wire::message::Role;

const SESSION: &str = "55555555-5555-5555-5555-555555555555";
const PROMPT_LINE: &str = r#"{"type":"user","uuid":"u1","timestamp":"2026-10-10T12:00:00.000Z","message":{"role":"user","content":"say hello"}}"#;
const REPLY_LINE: &str = r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","timestamp":"2026-10-10T12:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Hello there friend"}]}}"#;

#[derive(Clone, Default)]
struct Posts(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl capture::writer::CaptureTransport for Posts {
    async fn post(&self, body: String) -> Result<CaptureResult, capture::writer::PostError> {
        self.0.lock().push(body);
        Ok(CaptureResult {
            ok: true,
            conversation_id: "conv-1".to_owned(),
            inserted: 0,
            skipped: 0,
        })
    }
}

fn hook_in(
    home: &Path,
    posts: &Posts,
) -> Result<capture::hook::ClaudeHook<Posts>, config::ConfigError> {
    let env = config::Environment::from_vars([("HOME".to_owned(), home.display().to_string())]);
    let writer = capture::writer::CaptureWriter::new(
        posts.clone(),
        capture::spool::CaptureSpool::new(home.join("spool")),
        None,
        None,
    );
    Ok(capture::hook::ClaudeHook::new(
        writer,
        &config::Paths::new(&env)?,
    ))
}

#[cfg_attr(dylint_lib = "rr_lints", expect(sleep))]
async fn claude_writes_the_reply_late(transcript: &Path) -> std::io::Result<()> {
    tokio::time::sleep(Duration::from_millis(250)).await;
    std::fs::write(transcript, format!("{PROMPT_LINE}\n{REPLY_LINE}\n"))
}

#[tokio::test(start_paused = true)]
async fn a_stop_captures_the_reply_claude_writes_after_the_hook_starts()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let transcript = home.path().join(format!("{SESSION}.jsonl"));
    std::fs::write(&transcript, format!("{PROMPT_LINE}\n"))?;
    let posts = Posts::default();
    let hook = hook_in(home.path(), &posts)?;
    let now: DateTime<Utc> = "2026-10-10T12:00:02.000Z".parse()?;
    let stdin = format!(
        r#"{{"session_id":"{SESSION}","transcript_path":"{}","hook_event_name":"Stop","last_assistant_message":"Hello there friend"}}"#,
        transcript.display()
    );
    let (run, written) = tokio::join!(
        hook.run(&stdin, now, "1791656243919-30e4c5d70"),
        claude_writes_the_reply_late(&transcript)
    );
    written?;
    assert_eq!((run.exit, run.stderr.as_str()), (0, ""));
    let body = posts
        .0
        .lock()
        .first()
        .cloned()
        .ok_or("the hook posted nothing")?;
    let batch: CaptureBatchRequest = serde_json::from_str(&body)?;
    let replies: Vec<&str> = batch
        .messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .map(|message| message.content.as_str())
        .collect();
    assert_eq!(replies, ["Hello there friend"]);
    Ok(())
}
