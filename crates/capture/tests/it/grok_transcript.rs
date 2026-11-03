use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

use capture::listing::StoreRoots;
use capture::listing::StoreSessionLister;
use kernel::transcript::HarnessTranscriptTool;
use kernel::transcript::HarnessTranscriptTurn;
use kernel::transcript::SessionLister;
use kernel::transcript::SessionMessageRole;
use kernel::transcript::TranscriptRead;
use kernel::transcript::TranscriptToolStatus;
use kernel::transcript::TranscriptWindow;
use wire::harness::HarnessId;

const SESSION: &str = "33333333-3333-3333-3333-333333333333";
const CALL: &str = "call-ask-0";

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn updates_path(root: &Path) -> PathBuf {
    root.join("sessions")
        .join("%2Fwork%2Fdemo")
        .join(SESSION)
        .join("updates.jsonl")
}

fn update(at: i64, prompt: bool, body: &str) -> String {
    let prompt = if prompt {
        r#","promptId":"01a00000-0000-7000-8000-00000000aa01""#
    } else {
        ""
    };
    format!(
        r#"{{"method":"session/update","params":{{"_meta":{{"agentTimestampMs":{at}{prompt}}},"sessionId":"{SESSION}","update":{body}}}}}"#
    ) + "\n"
}

fn mid_turn() -> String {
    [
        update(
            1_791_720_001_000,
            false,
            r#"{"sessionUpdate":"hook_execution","event_name":"user_prompt_submit"}"#,
        ),
        update(
            1_791_720_002_000,
            false,
            r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"red or blue?"}}"#,
        ),
        update(
            1_791_720_003_000,
            true,
            r#"{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Ask the user."}}"#,
        ),
        update(
            1_791_720_004_000,
            true,
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"I'll ask which color you prefer."}}"#,
        ),
        update(
            1_791_720_005_000,
            true,
            &format!(
                r#"{{"sessionUpdate":"tool_call","toolCallId":"{CALL}","title":"ask_user_question","rawInput":{{"questions":[{{"question":"Red or blue?"}}]}}}}"#
            ),
        ),
        update(
            1_791_720_006_000,
            false,
            r#"{"sessionUpdate":"hook_execution","event_name":"pre_tool_use"}"#,
        ),
    ]
    .concat()
}

async fn turns(root: &Path) -> Result<Vec<HarnessTranscriptTurn>, Box<dyn std::error::Error>> {
    let lister = StoreSessionLister::new(StoreRoots {
        grok: Some(root.to_path_buf()),
        ..StoreRoots::default()
    });
    match lister
        .transcript(HarnessId::GrokBuild, SESSION, TranscriptWindow::Latest)
        .await?
    {
        TranscriptRead::Read(read) => Ok(read.turns),
        TranscriptRead::NoSource => Err("grok has a transcript source".into()),
    }
}

fn store(text: &str) -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = updates_path(dir.path());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(dir)
}

fn append(root: &Path, text: &str) -> TestResult {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(updates_path(root))?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

fn tools(turn: &HarnessTranscriptTurn) -> &[HarnessTranscriptTool] {
    turn.tools.as_deref().unwrap_or_default()
}

#[tokio::test]
async fn a_running_turn_is_read_with_its_open_tool_call() -> TestResult {
    let dir = store(&mid_turn())?;
    let turns = turns(dir.path()).await?;
    let [user, reply] = turns.as_slice() else {
        return Err(format!("expected a user turn and a reply, got {turns:?}").into());
    };
    assert_eq!(user.role, SessionMessageRole::User);
    assert_eq!(user.text, "red or blue?");
    assert_eq!(reply.role, SessionMessageRole::Assistant);
    assert_eq!(reply.text, "I'll ask which color you prefer.");
    assert_eq!(reply.thinking.as_deref(), Some("Ask the user."));
    assert!(!reply.complete);
    assert_eq!(reply.stop_reason, None);
    assert_eq!(reply.started_at_ms, Some(1_791_720_003_000));
    let [tool] = tools(reply) else {
        return Err(format!("expected one tool, got {reply:?}").into());
    };
    assert_eq!(tool.name, "ask_user_question");
    assert_eq!(tool.id.as_deref(), Some(CALL));
    assert_eq!(tool.status, TranscriptToolStatus::Running);
    let question = tool
        .input
        .as_ref()
        .and_then(|input| input.get("questions"))
        .and_then(|questions| questions.pointer("/0/question"))
        .and_then(|question| question.as_str());
    assert_eq!(question, Some("Red or blue?"));
    Ok(())
}

#[tokio::test]
async fn a_todo_list_keeps_its_items() -> TestResult {
    let dir = store(
        &[
            update(
                1_791_720_001_000,
                false,
                r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"check the date"}}"#,
            ),
            update(
                1_791_720_002_000,
                true,
                &format!(
                    r#"{{"sessionUpdate":"tool_call","toolCallId":"{CALL}","title":"todo_write","rawInput":{{"todos":[{{"id":"date","content":"Check the date","status":"in_progress"}}],"merge":false}}}}"#
                ),
            ),
        ]
        .concat(),
    )?;
    let turns = turns(dir.path()).await?;
    let item = turns
        .iter()
        .flat_map(tools)
        .find(|tool| tool.name == "todo_write")
        .and_then(|tool| tool.input.as_ref())
        .and_then(|input| input.get("todos"))
        .and_then(|todos| todos.pointer("/0/content"))
        .and_then(|content| content.as_str());
    assert_eq!(item, Some("Check the date"));
    Ok(())
}

#[tokio::test]
async fn the_answered_turn_settles_with_the_answer() -> TestResult {
    let dir = store(&mid_turn())?;
    append(
        dir.path(),
        &[
            update(
                1_791_720_007_000,
                true,
                &format!(
                    r#"{{"sessionUpdate":"tool_call_update","toolCallId":"{CALL}","status":"completed","content":[{{"type":"content","content":{{"type":"text","text":"Blue"}}}}]}}"#
                ),
            ),
            update(
                1_791_720_008_000,
                true,
                r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Blue it is."}}"#,
            ),
            update(
                1_791_720_009_000,
                false,
                r#"{"sessionUpdate":"turn_completed","prompt_id":"01a00000-0000-7000-8000-00000000aa01","stop_reason":"end_turn"}"#,
            ),
        ]
        .concat(),
    )?;
    let turns = turns(dir.path()).await?;
    let reply = turns.last().ok_or("no turns")?;
    assert_eq!(turns.len(), 2);
    assert_eq!(
        reply.text,
        "I'll ask which color you prefer.\n\nBlue it is."
    );
    assert!(reply.complete);
    assert_eq!(reply.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(reply.ended_at_ms, Some(1_791_720_009_000));
    let [tool] = tools(reply) else {
        return Err(format!("expected one tool, got {reply:?}").into());
    };
    assert_eq!(tool.status, TranscriptToolStatus::Done);
    assert_eq!(tool.result_text.as_deref(), Some("Blue"));
    Ok(())
}

#[tokio::test]
async fn a_later_prompt_follows_the_recorded_turns() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = updates_path(dir.path());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(
        testkit::recording("capture/stores/grok-updates-open.jsonl"),
        &path,
    )?;
    let turns = turns(dir.path()).await?;
    let roles: Vec<(SessionMessageRole, &str, bool)> = turns
        .iter()
        .map(|turn| (turn.role, turn.text.as_str(), turn.complete))
        .collect();
    assert_eq!(
        roles,
        vec![
            (SessionMessageRole::User, "list the files", false),
            (SessionMessageRole::Assistant, "Running ls.\n\nDone.", true),
            (SessionMessageRole::User, "now count them", false),
            (SessionMessageRole::Assistant, "Counting.", false),
        ]
    );
    let first = turns.get(1).ok_or("first reply")?;
    let statuses: Vec<(&str, TranscriptToolStatus)> = tools(first)
        .iter()
        .map(|tool| (tool.name.as_str(), tool.status))
        .collect();
    assert_eq!(
        statuses.last(),
        Some(&("write", TranscriptToolStatus::Error))
    );
    assert!(
        statuses
            .iter()
            .take(statuses.len().saturating_sub(1))
            .all(|(_, status)| *status == TranscriptToolStatus::Done),
        "{statuses:?}"
    );
    Ok(())
}
