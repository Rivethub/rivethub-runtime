use std::path::Path;

use capture::listing::StoreRoots;
use capture::listing::StoreSessionLister;
use kernel::transcript::HarnessTranscriptTool;
use kernel::transcript::HarnessTranscriptTurn;
use kernel::transcript::SessionLister;
use kernel::transcript::SessionMessageRole;
use kernel::transcript::ToolOutputRead;
use kernel::transcript::TranscriptBlock;
use kernel::transcript::TranscriptRead;
use kernel::transcript::TranscriptToolStatus;
use kernel::transcript::TranscriptWindow;
use wire::harness::HarnessId;

const SESSION: &str = "01a1dc07-9a45-75c0-be53-7dbb3d3d06e0";

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn item(second: u32, payload: &str) -> String {
    format!(
        r#"{{"timestamp":"2026-11-14T21:47:{second:02}.000Z","type":"response_item","payload":{payload}}}"#
    ) + "\n"
}

fn rollout() -> String {
    [
        format!(
            r#"{{"timestamp":"2026-11-14T21:47:00.000Z","type":"session_meta","payload":{{"id":"{SESSION}","cwd":"/home/person/proj"}}}}"#
        ) + "\n",
        item(
            1,
            r#"{"type":"message","id":"msg_1","role":"developer","content":[{"type":"input_text","text":"<skills_instructions>\nSkills.\n</skills_instructions>"}]}"#,
        ),
        item(
            2,
            r#"{"type":"message","id":"msg_2","role":"user","content":[{"type":"input_text","text":"Report the kernel."}]}"#,
        ),
        item(
            3,
            r#"{"type":"reasoning","id":"rs_1","summary":[],"content":[{"type":"reasoning_text","text":"The user wants the kernel version."}]}"#,
        ),
        item(
            4,
            r#"{"type":"message","id":"msg_3","role":"assistant","content":[{"type":"output_text","text":"I'll check the kernel."}]}"#,
        ),
        item(
            5,
            r#"{"type":"function_call","status":"completed","id":"fc_1","name":"exec_command","arguments":"{\"cmd\":\"uname -sr\"}","call_id":"call_a"}"#,
        ),
        item(
            6,
            r#"{"type":"function_call","status":"completed","id":"fc_2","name":"exec_command","arguments":"{\"cmd\":\"false\"}","call_id":"call_b"}"#,
        ),
        item(
            7,
            r#"{"type":"function_call_output","id":"fco_1","call_id":"call_a","output":"Linux 7.1.9-arch1-2\n"}"#,
        ),
        item(
            8,
            r#"{"type":"function_call_output","id":"fco_2","call_id":"call_b","output":"exit 1","error":"command failed"}"#,
        ),
        item(
            9,
            r#"{"type":"reasoning","id":"rs_2","summary":[],"content":[{"type":"reasoning_text","text":"Report it in one line."}]}"#,
        ),
        item(
            10,
            r#"{"type":"message","id":"msg_4","role":"assistant","content":[{"type":"output_text","text":"The kernel is Linux 7.1.9."}]}"#,
        ),
    ]
    .concat()
}

fn store(text: &str) -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let day = dir
        .path()
        .join("sessions")
        .join("2026")
        .join("11")
        .join("14");
    std::fs::create_dir_all(&day)?;
    std::fs::write(
        day.join(format!("rollout-2026-11-14T16-47-28-{SESSION}.jsonl")),
        text,
    )?;
    Ok(dir)
}

fn lister(root: &Path) -> StoreSessionLister {
    StoreSessionLister::new(StoreRoots {
        codex: Some(root.to_path_buf()),
        ..StoreRoots::default()
    })
}

async fn turns(root: &Path) -> Result<Vec<HarnessTranscriptTurn>, Box<dyn std::error::Error>> {
    match lister(root)
        .transcript(HarnessId::Codex, SESSION, TranscriptWindow::Latest)
        .await?
    {
        TranscriptRead::Read(read) => Ok(read.turns),
        TranscriptRead::NoSource => Err("codex has a transcript source".into()),
    }
}

fn tools(turn: &HarnessTranscriptTurn) -> &[HarnessTranscriptTool] {
    turn.tools.as_deref().unwrap_or_default()
}

#[tokio::test]
async fn a_codex_session_is_read_with_its_thinking_and_tool_calls() -> TestResult {
    let dir = store(&rollout())?;
    let turns = turns(dir.path()).await?;
    let [user, work, reply] = turns.as_slice() else {
        return Err(format!("expected a prompt, the work and the reply, got {turns:?}").into());
    };
    assert_eq!(user.role, SessionMessageRole::User);
    assert_eq!(user.text, "Report the kernel.");

    assert_eq!(work.role, SessionMessageRole::Assistant);
    assert_eq!(
        work.thinking.as_deref(),
        Some("The user wants the kernel version.")
    );
    assert_eq!(work.text, "I'll check the kernel.");
    assert_eq!(work.last_block, Some(TranscriptBlock::ToolResult));
    assert_eq!(work.started_at_ms, Some(1_794_692_823_000));
    assert_eq!(work.ended_at_ms, Some(1_794_692_828_000));
    let [first, second] = tools(work) else {
        return Err(format!("expected two tool calls, got {work:?}").into());
    };
    assert_eq!(first.name, "exec_command");
    assert_eq!(first.id.as_deref(), Some("fc_1"));
    assert_eq!(first.status, TranscriptToolStatus::Done);
    assert_eq!(
        first
            .args
            .as_ref()
            .and_then(|args| args.get("cmd"))
            .and_then(|cmd| cmd.as_str()),
        Some("uname -sr")
    );
    assert_eq!(second.id.as_deref(), Some("fc_2"));
    assert_eq!(second.status, TranscriptToolStatus::Error);

    assert_eq!(reply.role, SessionMessageRole::Assistant);
    assert_eq!(reply.thinking.as_deref(), Some("Report it in one line."));
    assert_eq!(reply.text, "The kernel is Linux 7.1.9.");
    assert_eq!(reply.last_block, Some(TranscriptBlock::Text));
    assert_eq!(tools(reply), []);
    Ok(())
}

#[tokio::test]
async fn a_codex_tool_call_gives_its_output_when_asked() -> TestResult {
    let dir = store(&rollout())?;
    let lister = lister(dir.path());
    let ToolOutputRead::Found(first) = lister
        .tool_output(HarnessId::Codex, SESSION, "fc_1")
        .await?
    else {
        return Err("the first call has an output".into());
    };
    assert_eq!(first.text, "Linux 7.1.9-arch1-2\n");
    assert!(!first.is_error);
    assert!(!first.truncated);
    let ToolOutputRead::Found(second) = lister
        .tool_output(HarnessId::Codex, SESSION, "fc_2")
        .await?
    else {
        return Err("the second call has an output".into());
    };
    assert_eq!(second.text, "exit 1");
    assert!(second.is_error);
    assert!(matches!(
        lister
            .tool_output(HarnessId::Codex, SESSION, "fc_9")
            .await?,
        ToolOutputRead::Missing
    ));
    Ok(())
}

#[tokio::test]
async fn a_call_without_an_output_stays_open() -> TestResult {
    let whole = rollout();
    let text = whole.lines().take(7).collect::<Vec<_>>().join("\n");
    let dir = store(&text)?;
    let turns = turns(dir.path()).await?;
    let [_, work] = turns.as_slice() else {
        return Err(format!("expected a prompt and the work, got {turns:?}").into());
    };
    assert_eq!(
        tools(work)
            .iter()
            .map(|tool| tool.status)
            .collect::<Vec<_>>(),
        vec![TranscriptToolStatus::Running, TranscriptToolStatus::Running]
    );
    Ok(())
}
