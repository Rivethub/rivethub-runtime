use jiff::Timestamp;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use testkit::Json;
use wire::capture::CaptureResult;

use capture::hook::store::StoreHarness;
use capture::hook::store::StoreHook;
use capture::listing::store_roots;
use capture::spool::CaptureSpool;
use capture::writer::CaptureTransport;
use capture::writer::CaptureWriter;
use capture::writer::PostError;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const THINKING: &str = "01a1db37-ac1c-7c90-8e50-ada758900361";
const PARENT: &str = "01a1db35-3246-7211-8e3b-57f76df009c0";

#[derive(Clone, Default)]
struct FakeTransport {
    posts: Arc<Mutex<Vec<String>>>,
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

async fn stop_rows(
    home: &Path,
    session: &str,
    fixture: &str,
) -> Result<Vec<(String, String, String)>, Box<dyn std::error::Error>> {
    let dir = home
        .join(".grok/sessions/%2Fhome%2Fuser%2Facmekit")
        .join(session);
    std::fs::create_dir_all(&dir)?;
    std::fs::copy(
        testkit::recording(&format!("capture/stores/grok-subagent/{fixture}")),
        dir.join("updates.jsonl"),
    )?;
    let env = config::Environment::from_vars([("HOME".to_owned(), home.display().to_string())]);
    let transport = FakeTransport::default();
    let hook = StoreHook::new(
        StoreHarness::GrokBuild,
        CaptureWriter::new(
            transport.clone(),
            CaptureSpool::new(home.join("spool")),
            None,
            None,
        ),
        &store_roots(&env),
        &config::Paths::new(&env)?,
    )
    .ok_or("grok store")?;
    let now: Timestamp = "2026-11-14T18:02:00Z".parse()?;
    let payload = format!(
        r#"{{"hook_event_name":"Stop","session_id":"{session}","cwd":"/home/user/acmekit"}}"#
    );
    assert_eq!(hook.run(&payload, now).await.stderr, "");
    let posts = transport.posts.lock().clone();
    let [body] = posts.as_slice() else {
        return Err(format!("expected one post, got {}", posts.len()).into());
    };
    let batch: Json = serde_json::from_str(body)?;
    Ok(batch
        .pointer("/messages")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .map(|message| {
            let text = |pointer: &str| {
                message
                    .pointer(pointer)
                    .and_then(Json::as_str)
                    .unwrap_or("-")
                    .to_owned()
            };
            (
                text("/role"),
                text("/tool_name"),
                text("/content").chars().take(60).collect(),
            )
        })
        .collect())
}

fn row(role: &str, tool: &str, content: &str) -> (String, String, String) {
    (
        role.to_owned(),
        tool.to_owned(),
        content.chars().take(60).collect(),
    )
}

#[tokio::test]
async fn grok_thought_chunks_are_thinking_rows_and_blank_replies_are_not_stored() -> TestResult {
    let home = tempfile::tempdir()?;
    let rows = stop_rows(home.path(), THINKING, "thinking-updates.jsonl").await?;
    assert_eq!(
        rows,
        vec![
            row(
                "user",
                "-",
                "Run the shell command uname -sr, then reply with its output in one line."
            ),
            row(
                "assistant",
                "-",
                "[thinking] The user is asking me to run `uname -sr` and provide its output as a single line."
            ),
            row(
                "tool",
                "run_terminal_command",
                "[tool] run_terminal_command"
            ),
            row(
                "assistant",
                "-",
                "[thinking] This is a simple task. Just report the output as is"
            ),
            row("assistant", "-", "[thinking] .\n"),
            row("assistant", "-", "Linux 6.8.0-acme"),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_grok_session_with_a_subagent_stores_no_blank_assistant_row() -> TestResult {
    let home = tempfile::tempdir()?;
    let rows = stop_rows(home.path(), PARENT, "parent-updates.jsonl").await?;
    let blank: Vec<_> = rows
        .iter()
        .filter(|(_, _, content)| content.trim().is_empty())
        .collect();
    assert_eq!(blank, Vec::<&(String, String, String)>::new());
    let tools: Vec<&str> = rows
        .iter()
        .filter(|(role, _, _)| role == "tool")
        .map(|(_, tool, _)| tool.as_str())
        .collect();
    assert_eq!(
        tools,
        vec!["read_file", "run_terminal_command", "spawn_subagent"]
    );
    let last = rows.last().ok_or("rows")?;
    assert_eq!(
        last,
        &row("assistant", "-", "\n\nLinux 6.8.0-acme | PELICAN-42 | 196")
    );
    Ok(())
}
