use jiff::Timestamp;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

use capture::readers::SkipRule;
use capture::readers::StoreReader;
use capture::readers::TranscriptSlice;
use kernel::transcript::TranscriptReader;
use wire::capture::CaptureMessage;
use wire::harness::HarnessId;
use wire::message::Role;
use wire::session_id::SessionId;

const SESSION: &str = "33333333-3333-3333-3333-333333333333";
const SETTLED: u64 = 28;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn session() -> SessionId {
    SessionId::new(HarnessId::GrokBuild, SESSION)
}

fn updates_path(root: &Path) -> PathBuf {
    root.join("sessions")
        .join("%2Fwork%2Fdemo")
        .join(SESSION)
        .join("updates.jsonl")
}

fn store() -> Result<(tempfile::TempDir, StoreReader), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = updates_path(dir.path());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(
        testkit::recording("capture/stores/grok-updates-open.jsonl"),
        &path,
    )?;
    let reader =
        StoreReader::open(HarnessId::GrokBuild, dir.path().to_path_buf()).ok_or("grok reader")?;
    Ok((dir, reader))
}

fn finish_turn(root: &Path) -> TestResult {
    let rest = std::fs::read(testkit::recording("capture/stores/grok-updates-rest.jsonl"))?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(updates_path(root))?;
    file.write_all(&rest)?;
    Ok(())
}

fn rows(slice: &TranscriptSlice) -> Vec<(String, Role, &str)> {
    slice
        .messages
        .iter()
        .map(|message| {
            (
                message.event_id.clone(),
                message.role,
                message.content.as_str(),
            )
        })
        .collect()
}

fn expected(rows: &[(i64, Role, &'static str)]) -> Vec<(String, Role, &'static str)> {
    rows.iter()
        .map(|(ordinal, role, content)| {
            (format!("grok-build:{SESSION}:{ordinal}"), *role, *content)
        })
        .collect()
}

#[derive(serde::Deserialize)]
struct Position {
    position: u64,
}

fn tool_at(slice: &TranscriptSlice, line: u64) -> Result<&CaptureMessage, String> {
    slice
        .messages
        .iter()
        .find(|message| {
            message
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.deserialize::<Position>().ok())
                .is_some_and(|record| record.position == line)
        })
        .ok_or_else(|| format!("no message from line {line}"))
}

#[tokio::test]
async fn grok_sessions_are_listed_from_their_update_logs() -> TestResult {
    let (dir, reader) = store()?;
    std::fs::create_dir_all(dir.path().join("sessions/%2Fwork%2Fdemo/notes"))?;
    std::fs::write(
        dir.path()
            .join("sessions/%2Fwork%2Fdemo/notes/updates.jsonl"),
        "",
    )?;
    assert_eq!(reader.sessions(dir.path()).await?, vec![session()]);
    Ok(())
}

#[tokio::test]
async fn closed_turns_are_captured_and_the_open_turn_is_held_back() -> TestResult {
    let (_dir, reader) = store()?;
    let slice = reader.messages(&session(), 0).await?;
    assert_eq!(
        rows(&slice),
        expected(&[
            (0, Role::User, "list the files"),
            (
                10_002,
                Role::Assistant,
                "[thinking] The user wants a listing."
            ),
            (10_003, Role::Assistant, "Running ls."),
            (10_006, Role::Tool, "[tool] run_terminal_command"),
            (10_008, Role::Tool, "[tool] read_file"),
            (10_010, Role::Tool, "[tool] grep"),
            (10_012, Role::Tool, "[tool] docs_search"),
            (10_014, Role::Tool, "[tool] ask_user_question"),
            (10_015, Role::Tool, "[tool] web_fetch"),
            (10_017, Role::Tool, "[tool] Web search:"),
            (10_020, Role::Assistant, "Done."),
            (10_021, Role::System, "[grok.memory_flush_started]"),
            (10_022, Role::System, "[grok.memory_flush_completed]"),
        ])
    );
    assert_eq!(slice.next_offset, SETTLED);
    let skipped: Vec<(u64, SkipRule)> = slice
        .skipped
        .iter()
        .map(|record| (record.position, record.rule))
        .collect();
    assert_eq!(skipped, vec![(27, SkipRule::UnknownRecordType)]);
    let again = reader.messages(&session(), SETTLED).await?;
    assert_eq!(again, TranscriptSlice::empty(SETTLED));
    Ok(())
}

#[tokio::test]
async fn the_held_turn_is_captured_once_it_completes() -> TestResult {
    let (dir, reader) = store()?;
    finish_turn(dir.path())?;
    let slice = reader.messages(&session(), SETTLED).await?;
    assert_eq!(
        rows(&slice),
        expected(&[
            (1_000_000, Role::User, "now count them"),
            (1_010_029, Role::Assistant, "Counting."),
            (1_010_030, Role::Assistant, "There are two files."),
        ])
    );
    assert_eq!(slice.next_offset, 32);
    let caught_up = reader.messages(&session(), slice.next_offset).await?;
    assert_eq!(caught_up, TranscriptSlice::empty(32));
    Ok(())
}

#[tokio::test]
async fn tool_results_use_the_readable_output_of_each_tool() -> TestResult {
    let (_dir, reader) = store()?;
    let slice = reader.messages(&session(), 0).await?;
    let results = [
        (7, "a.txt\n\n[exit_code=0]"),
        (9, "hello"),
        (11, "a.txt:1:hello\n"),
        (13, "[mcp docs/search] ERROR\nnot found"),
        (16, "page body"),
    ];
    for (line, result) in results {
        assert_eq!(tool_at(&slice, line)?.tool_result.as_deref(), Some(result));
    }
    let fallback = tool_at(&slice, 15)?.tool_result.clone().ok_or("result")?;
    assert!(fallback.contains(r#""answer":"yes""#), "{fallback}");
    let search = tool_at(&slice, 18)?
        .tool_result
        .clone()
        .ok_or("search result")?;
    assert!(search.contains(r#""query":"hello docs""#), "{search}");
    let bash = tool_at(&slice, 7)?;
    assert_eq!(bash.tool_name.as_deref(), Some("run_terminal_command"));
    let command = bash
        .tool_args
        .as_ref()
        .and_then(|arguments| arguments.get("command"))
        .and_then(|command| command.as_str());
    assert_eq!(command, Some("ls"));
    let orphan = tool_at(&slice, 16)?;
    assert_eq!(orphan.tool_name.as_deref(), Some("web_fetch"));
    assert!(orphan.tool_args.is_none());
    Ok(())
}

#[tokio::test]
async fn reads_carry_the_agent_clock_through_the_open_turn() -> TestResult {
    let (_dir, reader) = store()?;
    let turns = reader.read(&session()).await?;
    let first = turns.first().ok_or("no turns")?;
    assert_eq!(first.role, Role::User);
    assert_eq!(first.at, "2026-10-11T12:00:02Z".parse::<Timestamp>()?);
    assert_eq!(turns.len(), 15);
    let open = turns.last().ok_or("no open turn")?;
    assert_eq!(
        (open.role, open.content.as_str()),
        (Role::Assistant, "Counting.")
    );
    Ok(())
}
