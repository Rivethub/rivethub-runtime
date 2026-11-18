use jiff::Timestamp;
use std::path::Path;

use capture::event_id::OccurrenceKind;
use capture::event_id::occurrence_digest;
use capture::readers::StoreReader;
use capture::readers::TranscriptSlice;
use kernel::transcript::TranscriptReader;
use serde::Serialize;
use wire::JsonObject;
use wire::capture::CaptureMessage;
use wire::harness::HarnessId;
use wire::message::Role;
use wire::session_id::SessionId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TASK: &str = "local-agent-mode-sessions/account/org";
const META: &str = r#"{"sessionId":"local_0a1b2c3d-1111-2222-3333-444444444444","cliSessionId":"c1","title":"T","createdAt":1700000000000}"#;
const PROMPT: &str = r#"{"type":"user","uuid":"u1","timestamp":"2026-10-01T10:00:00Z","message":{"role":"user","content":"list files"}}"#;
const CALL: &str = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-10-01T10:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Looking."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#;
const RESULT: &str = r#"{"type":"user","uuid":"r1","timestamp":"2026-10-01T10:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"a.txt"}]}}"#;
const DONE: &str = r#"{"type":"assistant","timestamp":"2026-10-01T10:00:03Z","message":{"role":"assistant","content":[{"type":"text","text":"Done."}]}}"#;

#[derive(Serialize)]
struct Meta<'a> {
    source: &'a str,
    position: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_use_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_result: Option<bool>,
}

#[derive(Serialize)]
struct Command<'a> {
    command: &'a str,
}

fn write(root: &Path, relative: &str, text: &str) -> TestResult {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

fn transcript(lines: &[&str]) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

fn session() -> SessionId {
    SessionId::new(HarnessId::Cowork, "c1")
}

fn reader(root: &Path) -> Result<StoreReader, Box<dyn std::error::Error>> {
    StoreReader::open(HarnessId::Cowork, root.to_path_buf()).ok_or_else(|| "reader".into())
}

fn at(raw: &str) -> Result<Timestamp, Box<dyn std::error::Error>> {
    Ok(wire::time::parse_rfc3339(raw)?)
}

fn meta(
    position: u64,
    tool_use_id: Option<&str>,
    pending_result: Option<bool>,
) -> Result<Option<JsonObject>, Box<dyn std::error::Error>> {
    Ok(Some(JsonObject::from_serialize(&Meta {
        source: "cowork-transcript",
        position,
        tool_use_id,
        pending_result,
    })?))
}

fn text(
    event_id: &str,
    role: Role,
    content: &str,
    position: u64,
    stamp: &str,
) -> Result<CaptureMessage, Box<dyn std::error::Error>> {
    Ok(CaptureMessage {
        event_id: event_id.to_owned(),
        role,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: meta(position, None, None)?,
        created_at: Some(at(stamp)?),
    })
}

fn call(result: Option<&str>) -> Result<CaptureMessage, Box<dyn std::error::Error>> {
    Ok(CaptureMessage {
        event_id: "cowork:c1:tool:t1".to_owned(),
        role: Role::Tool,
        content: "[tool call] Bash".to_owned(),
        tool_name: Some("Bash".to_owned()),
        tool_args: Some(JsonObject::from_serialize(&Command { command: "ls" })?),
        tool_result: result.map(str::to_owned),
        metadata: meta(2, Some("t1"), result.is_none().then_some(true))?,
        created_at: Some(at("2026-10-01T10:00:01Z")?),
    })
}

fn returned(name: &str) -> Result<CaptureMessage, Box<dyn std::error::Error>> {
    Ok(CaptureMessage {
        event_id: "cowork:c1:tool:t1".to_owned(),
        role: Role::Tool,
        content: format!("[tool call] {name}"),
        tool_name: Some(name.to_owned()),
        tool_args: None,
        tool_result: Some("a.txt".to_owned()),
        metadata: meta(3, Some("t1"), Some(false))?,
        created_at: Some(at("2026-10-01T10:00:02Z")?),
    })
}

fn done(position: u64) -> Result<CaptureMessage, Box<dyn std::error::Error>> {
    let digest = occurrence_digest(OccurrenceKind::Assistant, "Done.", "", None);
    text(
        &format!("cowork:c1:occ:{digest}:0"),
        Role::Assistant,
        "Done.",
        position,
        "2026-10-01T10:00:03Z",
    )
}

#[tokio::test]
async fn task_directory_transcript_stitches_results_onto_calls() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        &format!("{TASK}/local_0a1b2c3d-1111-2222-3333-444444444444.json"),
        META,
    )?;
    write(
        dir.path(),
        &format!("{TASK}/0a1b2c3d/.claude/projects/-sessions-x/c1.jsonl"),
        &transcript(&[PROMPT, CALL, RESULT, DONE]),
    )?;
    let slice = reader(dir.path())?.messages(&session(), 0).await?;
    let expected = TranscriptSlice {
        messages: vec![
            text(
                "cowork:c1:u1",
                Role::User,
                "list files",
                1,
                "2026-10-01T10:00:00Z",
            )?,
            text(
                "cowork:c1:a1",
                Role::Assistant,
                "Looking.",
                2,
                "2026-10-01T10:00:01Z",
            )?,
            call(Some("a.txt"))?,
            done(4)?,
        ],
        next_offset: 4,
        skipped: Vec::new(),
    };
    assert_eq!(slice, expected);
    Ok(())
}

#[tokio::test]
async fn a_result_after_the_slice_fills_the_pending_call_id() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        &format!("{TASK}/local_0a1b2c3d-1111-2222-3333-444444444444.json"),
        META,
    )?;
    let file = format!("{TASK}/0a1b2c3d/.claude/projects/-sessions-x/c1.jsonl");
    write(dir.path(), &file, &transcript(&[PROMPT, CALL]))?;
    let reader = reader(dir.path())?;
    let first = reader.messages(&session(), 0).await?;
    assert_eq!(first.messages.last(), Some(&call(None)?));
    assert_eq!(first.next_offset, 2);
    write(
        dir.path(),
        &file,
        &transcript(&[PROMPT, CALL, RESULT, DONE]),
    )?;
    let second = reader.messages(&session(), first.next_offset).await?;
    assert_eq!(second.messages, vec![returned("Bash")?, done(4)?]);
    assert_eq!(second.next_offset, 4);
    Ok(())
}

#[tokio::test]
async fn sibling_and_cwd_task_directories_are_read() -> TestResult {
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    write(
        &root,
        &format!("{TASK}/local_old.json"),
        r#"{"cliSessionId":"c1","createdAt":1700000000000}"#,
    )?;
    write(
        &root,
        &format!("{TASK}/local_old/.claude/projects/-p/c1.jsonl"),
        &transcript(&[PROMPT]),
    )?;
    let cwd = root.join(TASK).join("custom").join("outputs");
    write(
        &root,
        &format!("{TASK}/local_new.json"),
        &format!(
            r#"{{"cliSessionId":"c2","createdAt":1700000000000,"cwd":"{}"}}"#,
            cwd.display()
        ),
    )?;
    write(
        &root,
        &format!("{TASK}/custom/.claude/projects/-p/c2.jsonl"),
        &transcript(&[RESULT]),
    )?;
    write(
        &root,
        "claude-code-sessions/local_none.json",
        r#"{"cliSessionId":"c3","createdAt":1}"#,
    )?;
    let reader = reader(&root)?;
    assert_eq!(
        reader.sessions(&root).await?,
        vec![session(), SessionId::new(HarnessId::Cowork, "c2")]
    );
    let old = reader.messages(&session(), 0).await?;
    assert_eq!(
        old.messages,
        vec![text(
            "cowork:c1:u1",
            Role::User,
            "list files",
            1,
            "2026-10-01T10:00:00Z"
        )?]
    );
    let orphan = reader
        .messages(&SessionId::new(HarnessId::Cowork, "c2"), 0)
        .await?;
    let mut expected = returned("tool")?;
    expected.event_id = "cowork:c2:tool:t1".to_owned();
    expected.metadata = meta(1, Some("t1"), Some(false))?;
    assert_eq!(orphan.messages, vec![expected]);
    Ok(())
}

#[tokio::test]
async fn lines_without_a_uuid_take_occurrence_ids() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        &format!("{TASK}/local_0a1b2c3d-1111-2222-3333-444444444444.json"),
        META,
    )?;
    let again = r#"{"type":"user","timestamp":"2026-10-01T10:00:00Z","message":{"role":"user","content":"again"}}"#;
    write(
        dir.path(),
        &format!("{TASK}/0a1b2c3d/.claude/projects/-sessions-x/c1.jsonl"),
        &transcript(&[again, again]),
    )?;
    let slice = reader(dir.path())?.messages(&session(), 0).await?;
    let digest = occurrence_digest(OccurrenceKind::User, "again", "", None);
    let ids: Vec<&str> = slice
        .messages
        .iter()
        .map(|message| message.event_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            format!("cowork:c1:occ:{digest}:0"),
            format!("cowork:c1:occ:{digest}:1")
        ]
    );
    let turns = reader(dir.path())?.read(&session()).await?;
    assert_eq!(turns.len(), 2);
    Ok(())
}
