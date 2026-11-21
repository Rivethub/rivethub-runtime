use std::fs;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use capture::backfill::Backfill;
use capture::harnesses::HarnessSetupService;
use capture::hook::antigravity::AntigravityHook;
use capture::listing::StoreRoots;
use capture::listing::StoreSessionLister;
use capture::listing::store_roots;
use capture::spool::CaptureSpool;
use capture::writer::CaptureTransport;
use capture::writer::CaptureWriter;
use capture::writer::PostError;
use config::Environment;
use jiff::Timestamp;
use kernel::harness_setup::RecallState;
use kernel::transcript::SessionLister as _;
use kernel::transcript::SessionMessageRole;
use kernel::transcript::ToolOutputRead;
use kernel::transcript::TranscriptRead;
use kernel::transcript::TranscriptToolStatus;
use kernel::transcript::TranscriptWindow;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::Mutex;
use wire::capture::CaptureResult;
use wire::harness::HarnessId;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const PARENT_ID: &str = "11111111-1111-4111-8111-111111111111";
const CHILD_ID: &str = "22222222-2222-4222-8222-222222222222";

#[derive(Debug, Default)]
struct RecordingTransport {
    bodies: Mutex<Vec<Value>>,
}

impl RecordingTransport {
    async fn batches(&self) -> Vec<Value> {
        self.bodies.lock().await.clone()
    }
}

#[derive(Clone)]
struct SharedTransport(Arc<RecordingTransport>);

#[async_trait]
impl CaptureTransport for SharedTransport {
    async fn post(&self, body: String) -> Result<CaptureResult, PostError> {
        self.0.post(body).await
    }
}

#[async_trait]
impl CaptureTransport for RecordingTransport {
    async fn post(&self, body: String) -> Result<CaptureResult, PostError> {
        let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let count = value
            .get("messages")
            .and_then(Value::as_array)
            .map_or(0, Vec::len) as u64;
        self.bodies.lock().await.push(value);
        Ok(CaptureResult {
            ok: true,
            conversation_id: "conv-1".to_owned(),
            inserted: count,
            skipped: 0,
        })
    }
}

fn now() -> TestResult<Timestamp> {
    Ok(wire::time::timestamp_at_utc(2026, 11, 21, 5, 10, 0)?)
}

fn at<T>(slice: &[T], index: usize) -> TestResult<&T> {
    slice
        .get(index)
        .ok_or_else(|| format!("missing index {index} in slice of len {}", slice.len()).into())
}

fn field<'a>(value: &'a Value, key: &str) -> TestResult<&'a Value> {
    value
        .get(key)
        .ok_or_else(|| format!("missing field {key}").into())
}

fn write_summaries_db(app_dir: &Path) -> TestResult {
    let db = app_dir.join("conversation_summaries.db");
    let conn = Connection::open(db)?;
    conn.execute_batch(
        "CREATE TABLE conversation_summaries (
            conversation_id TEXT PRIMARY KEY,
            title TEXT NOT NULL DEFAULT '',
            workspace_uris TEXT NOT NULL DEFAULT '[]',
            status TEXT NOT NULL DEFAULT '',
            agent_name TEXT NOT NULL DEFAULT '',
            parent_conversation_id TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE conversation_parents (
            conversation_id TEXT PRIMARY KEY,
            parent_id TEXT NOT NULL
        );",
    )?;
    conn.execute(
        "INSERT INTO conversation_summaries VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (
            PARENT_ID,
            "Antigravity Capture Integration",
            "[\"file:///work/rivethub\"]",
            "CASCADE_RUN_STATUS_IDLE",
            "",
            "",
        ),
    )?;
    conn.execute(
        "INSERT INTO conversation_summaries VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (
            CHILD_ID,
            "",
            "[\"file:///work/rivethub\"]",
            "CASCADE_RUN_STATUS_IDLE",
            "research",
            PARENT_ID,
        ),
    )?;
    Ok(())
}

fn write_transcript(app_dir: &Path, id: &str, name: &str, lines: &[&str]) -> TestResult {
    let logs = app_dir
        .join("brain")
        .join(id)
        .join(".system_generated/logs");
    fs::create_dir_all(&logs)?;
    fs::write(logs.join(name), format!("{}\n", lines.join("\n")))?;
    Ok(())
}

#[derive(Serialize)]
struct TestToolCall<'a> {
    name: &'a str,
    args: TestToolArgs<'a>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum TestToolArgs<'a> {
    Command {
        #[serde(rename = "CommandLine")]
        command_line: &'a str,
    },
    Read {
        #[serde(rename = "Path")]
        path: &'a str,
    },
    Subagent {
        #[serde(rename = "TypeName")]
        type_name: &'a str,
        #[serde(rename = "Role")]
        description: &'a str,
        #[serde(rename = "Prompt")]
        prompt: &'a str,
    },
}

#[derive(Serialize)]
struct TestStep<'a> {
    step_index: u64,
    source: &'a str,
    #[serde(rename = "type")]
    kind: &'a str,
    status: &'a str,
    created_at: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<TestToolCall<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TestHookPayload<'a> {
    conversation_id: &'a str,
    hook_event_name: &'a str,
    transcript_path: &'a Path,
    cwd: &'a str,
    model_name: &'a str,
    fully_idle: bool,
}

impl<'a> TestStep<'a> {
    const fn done(step_index: u64, source: &'a str, kind: &'a str, created_at: &'a str) -> Self {
        Self {
            step_index,
            source,
            kind,
            status: "DONE",
            created_at,
            thinking: None,
            error: None,
            content: None,
            tool_calls: Vec::new(),
        }
    }
}

fn parent_lines() -> TestResult<Vec<String>> {
    let subagent_output = format!(
        "Created the following subagent:\n{{\n  \"conversationId\":  \"{CHILD_ID}\",\n  \"logAbsoluteUri\":  \"file:///tmp/x\"\n}}"
    );
    let steps = [
        TestStep {
            content: Some(
                "<USER_REQUEST>\n<t3_code_instructions>\nInternal rules\n</t3_code_instructions>\n\n<user_request>\nWire up capture for Antigravity\n</user_request><runtime_info>info</runtime_info>\n<pull_request_linking>pr</pull_request_linking>\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nTime\n</ADDITIONAL_METADATA>",
            ),
            ..TestStep::done(0, "USER_EXPLICIT", "USER_INPUT", "2026-11-21T05:00:00Z")
        },
        TestStep {
            thinking: Some("Let us inspect the repo and spawn a research subagent."),
            tool_calls: vec![
                TestToolCall {
                    name: "run_command",
                    args: TestToolArgs::Command {
                        command_line: "ls -la",
                    },
                },
                TestToolCall {
                    name: "read_file",
                    args: TestToolArgs::Read {
                        path: "/missing.txt",
                    },
                },
                TestToolCall {
                    name: "invoke_subagent",
                    args: TestToolArgs::Subagent {
                        type_name: "research",
                        description: "Capture Researcher",
                        prompt: "Check hooks.md",
                    },
                },
            ],
            ..TestStep::done(1, "MODEL", "PLANNER_RESPONSE", "2026-11-21T05:00:01Z")
        },
        TestStep {
            content: Some("Cargo.toml\ncrates/"),
            ..TestStep::done(2, "MODEL", "GENERIC", "2026-11-21T05:00:02Z")
        },
        TestStep {
            status: "ERROR",
            error: Some("file does not exist"),
            content: Some("Encountered error: file does not exist"),
            ..TestStep::done(3, "MODEL", "GENERIC", "2026-11-21T05:00:03Z")
        },
        TestStep {
            content: Some(&subagent_output),
            ..TestStep::done(4, "MODEL", "GENERIC", "2026-11-21T05:00:04Z")
        },
        TestStep {
            content: Some("<SYSTEM_MESSAGE>\nReminder: work efficiently.\n</SYSTEM_MESSAGE>"),
            ..TestStep::done(5, "SYSTEM", "SYSTEM_MESSAGE", "2026-11-21T05:00:05Z")
        },
        TestStep {
            content: Some("All set."),
            ..TestStep::done(6, "MODEL", "PLANNER_RESPONSE", "2026-11-21T05:00:06Z")
        },
    ];
    steps
        .iter()
        .map(|step| Ok(serde_json::to_string(step)?))
        .collect()
}

fn child_lines() -> TestResult<Vec<String>> {
    let prompt = format!(
        "The following is a <SYSTEM_MESSAGE>\n\n<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-21T05:00:04Z sender={PARENT_ID} priority=MESSAGE_PRIORITY_HIGH content=Check hooks.md and report back\n</SYSTEM_MESSAGE>"
    );
    let steps = [
        TestStep {
            content: Some(&prompt),
            ..TestStep::done(0, "SYSTEM", "SYSTEM_MESSAGE", "2026-11-21T05:00:04Z")
        },
        TestStep {
            thinking: Some("Reading hooks.md"),
            content: Some("hooks.json uses camelCase payloads."),
            ..TestStep::done(1, "MODEL", "PLANNER_RESPONSE", "2026-11-21T05:00:05Z")
        },
    ];
    steps
        .iter()
        .map(|step| Ok(serde_json::to_string(step)?))
        .collect()
}

fn assert_parent_batch(parent: &Value) -> TestResult {
    assert_eq!(
        field(parent, "session_key")?,
        &format!("antigravity:{PARENT_ID}")
    );
    assert_eq!(field(parent, "agent")?, "rivet-gemini");
    assert_eq!(field(parent, "channel")?, "antigravity");
    assert_eq!(field(parent, "title")?, "Antigravity Capture Integration");
    assert!(parent.get("finalize").is_none());
    let msgs = field(parent, "messages")?
        .as_array()
        .ok_or("expected messages array")?;
    assert_eq!(field(at(msgs, 0)?, "role")?, "user");
    assert_eq!(
        field(at(msgs, 0)?, "content")?,
        "Wire up capture for Antigravity"
    );
    assert_eq!(field(at(msgs, 1)?, "role")?, "assistant");
    assert_eq!(
        field(at(msgs, 1)?, "content")?,
        "[thinking] Let us inspect the repo and spawn a research subagent."
    );
    assert_eq!(field(at(msgs, 2)?, "role")?, "tool");
    assert_eq!(field(at(msgs, 2)?, "content")?, "[tool] run_command");
    assert_eq!(field(at(msgs, 5)?, "content")?, "[tool-result] run_command");
    assert_eq!(field(at(msgs, 5)?, "tool_result")?, "Cargo.toml\ncrates/");
    assert_eq!(field(at(msgs, 6)?, "content")?, "[tool-failure] read_file");
    assert_eq!(field(at(msgs, 8)?, "role")?, "assistant");
    assert_eq!(field(at(msgs, 8)?, "content")?, "All set.");
    Ok(())
}

fn assert_child_batch(child: &Value) -> TestResult {
    assert_eq!(
        field(child, "session_key")?,
        &format!("antigravity:{CHILD_ID}")
    );
    assert_eq!(field(child, "finalize")?, true);
    let settings = field(child, "settings")?;
    assert_eq!(field(settings, "parent_session_id")?, PARENT_ID);
    assert_eq!(field(settings, "agent_id")?, CHILD_ID);
    assert_eq!(field(settings, "agent_type")?, "research");
    assert_eq!(field(settings, "description")?, "Capture Researcher");
    assert_eq!(
        field(settings, "tool_use_id")?,
        &format!("antigravity:{PARENT_ID}:step:1:tool:2")
    );
    let c_msgs = field(child, "messages")?
        .as_array()
        .ok_or("expected child messages array")?;
    assert_eq!(field(at(c_msgs, 0)?, "role")?, "user");
    assert_eq!(
        field(at(c_msgs, 0)?, "content")?,
        "Check hooks.md and report back"
    );
    assert_eq!(
        field(at(c_msgs, 1)?, "content")?,
        "[thinking] Reading hooks.md"
    );
    assert_eq!(
        field(at(c_msgs, 2)?, "content")?,
        "hooks.json uses camelCase payloads."
    );
    Ok(())
}

#[tokio::test]
async fn stop_hook_captures_parent_and_linked_subagent_from_full_transcript() -> TestResult {
    let home = TempDir::new()?;
    let app_dir = home.path().join(".gemini/antigravity-cli");
    fs::create_dir_all(&app_dir)?;
    write_summaries_db(&app_dir)?;
    let p_lines = parent_lines()?;
    let p_refs: Vec<&str> = p_lines.iter().map(String::as_str).collect();
    write_transcript(&app_dir, PARENT_ID, "transcript_full.jsonl", &p_refs)?;
    write_transcript(
        &app_dir,
        PARENT_ID,
        "transcript.jsonl",
        &["{\"truncated\":true}"],
    )?;
    let c_lines = child_lines()?;
    let c_refs: Vec<&str> = c_lines.iter().map(String::as_str).collect();
    write_transcript(&app_dir, CHILD_ID, "transcript_full.jsonl", &c_refs)?;

    let env = Environment::from_vars([("HOME".as_ref(), home.path().as_os_str())]);
    let paths = config::Paths::new(&env)?;
    let transport = Arc::new(RecordingTransport::default());
    let writer = CaptureWriter::new(
        SharedTransport(Arc::clone(&transport)),
        CaptureSpool::new(paths.state_dir().join("spool")),
        None,
        None,
    );
    let hook = AntigravityHook::new(writer, &store_roots(&env), &paths);

    let compact_path = app_dir
        .join("brain")
        .join(PARENT_ID)
        .join(".system_generated/logs/transcript.jsonl");
    let stdin = serde_json::to_string(&TestHookPayload {
        conversation_id: PARENT_ID,
        hook_event_name: "Stop",
        transcript_path: &compact_path,
        cwd: "/work/rivethub",
        model_name: "gemini-4-neon",
        fully_idle: true,
    })?;

    let run = hook.run(&stdin, now()?).await;
    assert_eq!(run.exit, 0);
    assert_eq!(run.stderr, "");

    let batches = transport.batches().await;
    assert_eq!(batches.len(), 2, "expected parent + child subagent batches");
    assert_parent_batch(at(&batches, 0)?)?;
    assert_child_batch(at(&batches, 1)?)?;

    let again = hook.run(&stdin, now()?).await;
    assert_eq!(again.exit, 0);
    assert_eq!(
        transport.batches().await.len(),
        3,
        "child should not re-post when unchanged"
    );
    Ok(())
}

#[tokio::test]
async fn backfill_and_history_read_antigravity_sessions_and_tool_outputs() -> TestResult {
    let home = TempDir::new()?;
    let app_dir = home.path().join(".gemini/antigravity-cli");
    fs::create_dir_all(&app_dir)?;
    write_summaries_db(&app_dir)?;
    let p_lines = parent_lines()?;
    let p_refs: Vec<&str> = p_lines.iter().map(String::as_str).collect();
    write_transcript(&app_dir, PARENT_ID, "transcript_full.jsonl", &p_refs)?;
    let c_lines = child_lines()?;
    let c_refs: Vec<&str> = c_lines.iter().map(String::as_str).collect();
    write_transcript(&app_dir, CHILD_ID, "transcript_full.jsonl", &c_refs)?;

    let roots = StoreRoots {
        antigravity: vec![app_dir],
        ..StoreRoots::default()
    };

    let lister = StoreSessionLister::new(roots.clone());
    let sessions = lister.harness(HarnessId::Antigravity, 50).await?;
    assert_eq!(sessions.len(), 2);

    let read = lister
        .transcript(HarnessId::Antigravity, PARENT_ID, TranscriptWindow::Latest)
        .await?;
    let TranscriptRead::Read(transcript) = read else {
        return Err("expected transcript".into());
    };
    assert_eq!(at(&transcript.turns, 0)?.role, SessionMessageRole::User);
    assert_eq!(
        at(&transcript.turns, 0)?.text,
        "Wire up capture for Antigravity"
    );
    let tools = at(&transcript.turns, 1)?
        .tools
        .as_ref()
        .ok_or("expected tools")?;
    assert_eq!(tools.len(), 3);
    assert_eq!(at(tools, 0)?.status, TranscriptToolStatus::Done);
    assert_eq!(at(tools, 1)?.status, TranscriptToolStatus::Error);

    let out = lister
        .tool_output(
            HarnessId::Antigravity,
            PARENT_ID,
            &format!("antigravity:{PARENT_ID}:step:1:tool:0"),
        )
        .await?;
    let ToolOutputRead::Found(out) = out else {
        return Err("expected tool output".into());
    };
    assert_eq!(out.text, "Cargo.toml\ncrates/");
    assert!(!out.is_error);

    let writer = CaptureWriter::new(
        RecordingTransport::default(),
        CaptureSpool::new(home.path().join("spool")),
        None,
        None,
    );
    let totals = Backfill::new(writer, roots)
        .run(&[HarnessId::Antigravity], false, now()?)
        .await;
    assert_eq!(totals.len(), 1);
    let total = at(&totals, 0)?;
    assert_eq!(total.sessions, 2);
    assert_eq!(total.failures, [] as [capture::backfill::Failure; 0]);
    assert!(total.added >= 12);
    Ok(())
}

#[test]
fn antigravity_harness_connect_and_disconnect_preserves_other_entries() -> TestResult {
    let home = TempDir::new()?;
    let config_dir = home.path().join(".gemini/config");
    fs::create_dir_all(&config_dir)?;
    let existing_hooks = "{\n  \"user-linter\": {\n    \"Stop\": [\n      {\n        \"type\": \"command\",\n        \"command\": \"./lint.sh\"\n      }\n    ]\n  }\n}\n";
    fs::write(config_dir.join("hooks.json"), existing_hooks)?;
    fs::write(config_dir.join("mcp_config.json"), "")?;

    let env = Environment::from_vars([("HOME".as_ref(), home.path().as_os_str())]);
    let service = HarnessSetupService::new(
        &env,
        Path::new("/usr/local/bin/rh").to_path_buf(),
        home.path().join(".rivethub/backups"),
    );

    let before = service
        .probe_now()
        .into_iter()
        .find(|p| p.id == "antigravity")
        .ok_or("missing antigravity probe")?;
    assert!(before.found);
    assert!(!before.capture_installed);
    assert_eq!(before.recall.state, RecallState::NotConnected);
    assert!(before.can_connect);

    let connected = service.connect_now("antigravity", None, false, now()?)?;
    assert!(connected.skipped.is_none());
    let hooks_text = fs::read_to_string(config_dir.join("hooks.json"))?;
    let hooks_json: Value = serde_json::from_str(&hooks_text)?;
    assert!(hooks_json.get("user-linter").is_some());
    assert_eq!(
        hooks_json.pointer("/rivethub-capture/Stop/0/command"),
        Some(&Value::String(
            "/usr/local/bin/rh capture hook --harness antigravity".to_owned()
        ))
    );
    assert_eq!(
        hooks_json.pointer("/rivethub-capture/PostInvocation/0/command"),
        Some(&Value::String(
            "/usr/local/bin/rh capture hook --harness antigravity".to_owned()
        ))
    );

    let mcp_text = fs::read_to_string(config_dir.join("mcp_config.json"))?;
    let mcp_json: Value = serde_json::from_str(&mcp_text)?;
    assert_eq!(
        mcp_json.pointer("/mcpServers/rivethub/command"),
        Some(&Value::String("/usr/local/bin/rh".to_owned()))
    );
    assert_eq!(
        mcp_json.pointer("/mcpServers/rivethub/args/0"),
        Some(&Value::String("mcp".to_owned()))
    );

    let after = service
        .probe_now()
        .into_iter()
        .find(|p| p.id == "antigravity")
        .ok_or("missing antigravity probe")?;
    assert!(after.capture_installed);
    assert_eq!(after.recall.state, RecallState::Connected);
    assert!(!after.can_connect);

    let again = service.connect_now("antigravity", None, false, now()?)?;
    assert_eq!(again.outcome.diff, "");

    service.disconnect_now("antigravity", None, false, now()?)?;
    let hooks_after: Value =
        serde_json::from_str(&fs::read_to_string(config_dir.join("hooks.json"))?)?;
    assert!(hooks_after.get("user-linter").is_some());
    assert!(hooks_after.get("rivethub-capture").is_none());
    let mcp_after = fs::read_to_string(config_dir.join("mcp_config.json"))?;
    assert!(!mcp_after.contains("rivethub"));
    Ok(())
}
