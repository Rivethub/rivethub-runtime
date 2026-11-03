#![cfg(test)]
use std::fs;
use std::io::Write as _;
use std::path::Path;

use capture::listing::StoreRoots;
use capture::listing::StoreSessionLister;
use kernel::transcript::HarnessTranscriptTurn;
use kernel::transcript::SessionLister;
use kernel::transcript::StoreTranscript;
use kernel::transcript::TranscriptBlock;
use kernel::transcript::TranscriptRead;
use kernel::transcript::TranscriptToolStatus;
use kernel::transcript::TranscriptWindow;
use wire::harness::HarnessId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const U1: &str = "00000000-0000-4000-8000-000000000001";
const OMP: &str = "33333333-3333-3333-3333-333333333333";

fn write(root: &Path, relative: &str, content: &str) -> Result<std::path::PathBuf, std::io::Error> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, content)?;
    Ok(path)
}

fn claude(root: &Path) -> StoreSessionLister {
    StoreSessionLister::new(StoreRoots {
        claude: Some(root.to_path_buf()),
        ..StoreRoots::default()
    })
}

async fn read(lister: &StoreSessionLister, harness: HarnessId, id: &str) -> StoreTranscript {
    read_window(lister, harness, id, TranscriptWindow::Latest).await
}

async fn read_window(
    lister: &StoreSessionLister,
    harness: HarnessId,
    id: &str,
    window: TranscriptWindow,
) -> StoreTranscript {
    match lister
        .transcript(harness, id, window)
        .await
        .expect("transcript")
    {
        TranscriptRead::Read(read) => read,
        TranscriptRead::NoSource => panic!("{harness} has a transcript source"),
    }
}

#[tokio::test]
async fn dsh_has_no_transcript_source() {
    let lister = StoreSessionLister::new(StoreRoots::default());
    assert!(matches!(
        lister
            .transcript(HarnessId::Dsh, U1, TranscriptWindow::Latest)
            .await,
        Ok(TranscriptRead::NoSource)
    ));
}

async fn claude_read(lines: &[&str]) -> StoreTranscript {
    let dir = tempfile::tempdir().expect("store");
    write(
        dir.path(),
        &format!("projects/-home-user/{U1}.jsonl"),
        &lines.join("\n"),
    )
    .expect("transcript file");
    read(&claude(dir.path()), HarnessId::ClaudeCode, U1).await
}

async fn claude_turns(lines: &[&str]) -> Vec<HarnessTranscriptTurn> {
    claude_read(lines).await.turns
}

fn first_tool(turns: &[HarnessTranscriptTurn]) -> kernel::transcript::HarnessTranscriptTool {
    turns
        .first()
        .and_then(|turn| turn.tools.as_ref())
        .and_then(|tools| tools.first())
        .cloned()
        .expect("first tool")
}

fn first_turn(turns: &[HarnessTranscriptTurn]) -> &HarnessTranscriptTurn {
    turns.first().expect("first turn")
}

async fn claude_json(lines: &[&str]) -> String {
    serde_json::to_string(&claude_turns(lines).await).expect("turns json")
}

fn user(content: &str) -> String {
    format!(
        r#"{{"type":"user","message":{{"role":"user","content":{}}}}}"#,
        serde_json::to_string(content).expect("text")
    )
}

fn assistant(content: &str, stop: &str) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"role":"assistant","content":{content},"stop_reason":"{stop}"}}}}"#
    )
}

fn tool_use(id: &str, name: &str, input: &str) -> String {
    assistant(
        &format!(r#"[{{"type":"tool_use","id":"{id}","name":"{name}","input":{input}}}]"#),
        "tool_use",
    )
}

#[tokio::test]
async fn b5_1_thinking_text_and_usage_fold_into_one_complete_turn() {
    let reply = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"hello"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"cache_read_input_tokens":5,"output_tokens":3}}}"#;
    assert_eq!(
        claude_json(&[&user("hi"), reply]).await,
        r#"[{"role":"user","text":"hi"},{"role":"assistant","text":"hello","thinking":"x","usage":{"promptTokens":15,"completionTokens":3,"cachedTokens":5},"stopReason":"end_turn","lastBlock":"text","complete":true}]"#
    );
}

#[tokio::test]
async fn b5_2_a_tool_round_trip_stays_inside_the_assistant_turn() {
    let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"a.txt"}]}}"#;
    let done = assistant(r#"[{"type":"text","text":"done"}]"#, "end_turn");
    assert_eq!(
        claude_json(&[
            &tool_use("t1", "Bash", r#"{"command":"ls"}"#),
            result,
            &done
        ])
        .await,
        r#"[{"role":"assistant","text":"done","tools":[{"name":"Bash","status":"done","args":{"command":"ls"},"id":"t1"}],"stopReason":"end_turn","lastBlock":"text","complete":true}]"#
    );
}

#[tokio::test]
async fn b5_3_command_wrappers_are_not_user_turns() {
    assert_eq!(
        claude_turns(&[&user("<command-name>/clear</command-name>")]).await,
        Vec::new()
    );
}

#[tokio::test]
async fn b5_4_bare_slash_commands_are_not_user_turns() {
    assert_eq!(
        claude_turns(&[&user("/compact"), &user("/model opus")]).await,
        Vec::new()
    );
    assert_eq!(
        claude_json(&[&user("/model\nwhy is it slow")]).await,
        r#"[{"role":"user","text":"/model\nwhy is it slow"}]"#
    );
}

#[tokio::test]
async fn b5_5_the_paste_wrapper_is_stripped() {
    assert_eq!(
        claude_json(&[&user(
            "<pasted_content id=\"7\">\nabc\n</pasted_content id=\"7\">"
        )])
        .await,
        r#"[{"role":"user","text":"abc"}]"#
    );
}

#[tokio::test]
async fn b5_6_a_compact_boundary_between_complete_turns_is_a_marker() {
    let answer = assistant(r#"[{"type":"text","text":"a1"}]"#, "end_turn");
    let boundary = r#"{"type":"system","subtype":"compact_boundary","compactMetadata":{"preTokens":180000,"postTokens":12000}}"#;
    let summary = r#"{"type":"user","isCompactSummary":true,"message":{"role":"user","content":"summary of the conversation"}}"#;
    let turns = claude_turns(&[&user("q1"), &answer, boundary, summary, &user("q2")]).await;
    assert_eq!(turns.len(), 4);
    assert_eq!(
        serde_json::to_string(turns.get(2).expect("marker")).expect("marker"),
        r#"{"role":"assistant","text":"Conversation compacted (180k tokens → 12k tokens)","usage":{"promptTokens":12000,"completionTokens":0,"cachedTokens":0},"stopReason":"end_turn","lastBlock":"text","complete":true,"compact":true}"#
    );
}

#[tokio::test]
async fn a_boundary_after_a_user_turn_or_inside_an_open_turn_is_skipped() {
    let boundary = r#"{"type":"system","subtype":"compact_boundary"}"#;
    let open = tool_use("t1", "Bash", "{}");
    assert_eq!(
        claude_turns(&[&user("q1"), boundary, &open, boundary])
            .await
            .len(),
        2
    );
    let complete = assistant(r#""all good""#, "end_turn");
    let marked = claude_turns(&[&user("q1"), &complete, boundary]).await;
    assert_eq!(
        marked.get(2).map(|turn| turn.text.as_str()),
        Some("Conversation compacted")
    );
    assert_eq!(marked.get(2).and_then(|turn| turn.usage.as_ref()), None);
}

#[tokio::test]
async fn a_turn_counts_each_model_call_once_and_its_last_call_is_the_context() {
    let call = |id: &str, block: &str, input: u64, output: u64| {
        format!(
            r#"{{"type":"assistant","message":{{"id":"{id}","role":"assistant","content":[{block}],"usage":{{"input_tokens":{input},"cache_read_input_tokens":40000,"output_tokens":{output}}}}}}}"#
        )
    };
    let read = claude_read(&[
        &user("q"),
        &call("m1", r#"{"type":"thinking","thinking":"t"}"#, 2, 400),
        &call("m1", r#"{"type":"text","text":"a"}"#, 2, 400),
        &call("m2", r#"{"type":"text","text":"b"}"#, 9000, 250),
    ])
    .await;
    let usage = read.turns.get(1).and_then(|turn| turn.usage.as_ref());
    assert_eq!(usage.map(|usage| usage.completion_tokens), Some(650));
    assert_eq!(read.context, Some(49_250));
}

#[tokio::test]
async fn token_labels_use_k_and_m_suffixes() {
    let boundary = r#"{"type":"system","subtype":"compact_boundary","compactMetadata":{"preTokens":1250000,"postTokens":900}}"#;
    let answer = assistant(r#""ok""#, "end_turn");
    let turns = claude_turns(&[&user("q"), &answer, boundary]).await;
    assert_eq!(
        turns.get(2).map(|turn| turn.text.as_str()),
        Some("Conversation compacted (1.3M tokens → 900 tokens)")
    );
}

#[tokio::test]
async fn b5_7_a_large_file_reads_its_tail_and_drops_parent_sidechains() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(format!("projects/-home-user/{U1}.jsonl"));
    fs::create_dir_all(path.parent().ok_or("parent")?)?;
    let mut file = std::io::BufWriter::new(fs::File::create(&path)?);
    writeln!(file, "{}", user("early"))?;
    let filler = format!(r#"{{"type":"progress","data":"{}"}}"#, "x".repeat(1024));
    for _ in 0..9 * 1024 {
        writeln!(file, "{filler}")?;
    }
    writeln!(
        file,
        r#"{{"type":"user","isSidechain":true,"message":{{"content":"side"}}}}"#
    )?;
    writeln!(file, "{}", user("late"))?;
    drop(file);
    let lister = claude(dir.path());
    let read = read(&lister, HarnessId::ClaudeCode, U1).await;
    assert_eq!(
        serde_json::to_string(&read.turns)?,
        r#"[{"role":"user","text":"late"}]"#
    );
    let before = read.earlier.ok_or("a tail read leaves earlier turns")?;
    let page = read_window(
        &lister,
        HarnessId::ClaudeCode,
        U1,
        TranscriptWindow::Before(before),
    )
    .await;
    assert_eq!(
        (serde_json::to_string(&page.turns)?, page.earlier),
        (r#"[{"role":"user","text":"early"}]"#.to_owned(), None)
    );
    Ok(())
}

#[tokio::test]
async fn an_all_sidechain_file_keeps_its_sidechain_lines() {
    let line = r#"{"type":"user","isSidechain":true,"message":{"content":"side"}}"#;
    assert_eq!(
        claude_json(&[line]).await,
        r#"[{"role":"user","text":"side"}]"#
    );
}

#[tokio::test]
async fn b5_8_an_errored_tool_result_marks_the_tool() {
    let result = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"exit 1"}]}}"#;
    let turns = claude_turns(&[&tool_use("t1", "Bash", r#"{"command":"false"}"#), result]).await;
    let turn = turns.first().expect("assistant turn");
    let tool = turn
        .tools
        .as_ref()
        .and_then(|tools| tools.first())
        .expect("tool");
    assert_eq!(tool.status, TranscriptToolStatus::Error);
    assert_eq!(turn.last_block, Some(TranscriptBlock::ToolResult));
    assert!(!turn.complete);
}

#[tokio::test]
async fn b5_9_a_large_prompt_tool_input_is_replaced_by_a_marker() {
    let question = "q".repeat(9 * 1024);
    let big = tool_use(
        "t1",
        "AskUserQuestion",
        &format!(r#"{{"question":"{question}"}}"#),
    );
    let turns = claude_turns(&[&big]).await;
    let tool = first_tool(&turns);
    assert_eq!(
        serde_json::to_string(&tool.input).expect("input"),
        r#"{"truncated":true}"#
    );
    assert_eq!(
        serde_json::to_string(&tool.args).expect("args"),
        format!(r#"{{"question":"{}…"}}"#, "q".repeat(200))
    );
    let small = tool_use("t2", " ask_user ", r#"{"question":"ok?"}"#);
    let answer = r#"{"type":"user","toolUseResult":{"answers":["yes"]},"message":{"content":[{"type":"tool_result","tool_use_id":"t2"}]}}"#;
    let turns = claude_turns(&[&small, answer]).await;
    let tool = first_tool(&turns);
    assert_eq!(
        serde_json::to_string(&tool.input).expect("input"),
        r#"{"question":"ok?"}"#
    );
    assert_eq!(tool.result_text.as_deref(), Some(r#"{"answers":["yes"]}"#));
}

#[tokio::test]
async fn args_keep_the_first_twelve_primitives_and_skip_nested_values() {
    let mut fields = vec![r#""nested":{"a":1}"#.to_owned()];
    fields.extend((0..14).map(|index| format!(r#""k{index:02}":{index}"#)));
    let call = tool_use("t1", "Edit", &format!("{{{}}}", fields.join(",")));
    let turns = claude_turns(&[&call]).await;
    let args = first_tool(&turns).args.expect("args");
    assert_eq!(args.as_map().len(), 12);
    assert!(args.get("nested").is_none());
    assert!(args.get("k11").is_some());
    assert!(args.get("k12").is_none());
}

#[tokio::test]
async fn long_thinking_keeps_its_tail() {
    let thinking = format!("{}{}", "a".repeat(100), "b".repeat(8000));
    let reply = assistant(
        &format!(r#"[{{"type":"thinking","thinking":"{thinking}"}}]"#),
        "end_turn",
    );
    let turns = claude_turns(&[&reply]).await;
    assert_eq!(
        first_turn(&turns).thinking.as_deref(),
        Some(format!("…{}", "b".repeat(8000)).as_str())
    );
    assert!(!first_turn(&turns).complete);
}

#[tokio::test]
async fn meta_lines_and_unparsable_lines_are_skipped() {
    let meta = r#"{"type":"user","isMeta":true,"message":{"content":"meta"}}"#;
    assert_eq!(
        claude_json(&[meta, "{not json", &user("real")]).await,
        r#"[{"role":"user","text":"real"}]"#
    );
}

#[tokio::test]
async fn a_subagent_id_reads_its_agent_file() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        &format!("projects/-home-user/{U1}/subagents/agent-a1.jsonl"),
        r#"{"type":"user","isSidechain":true,"message":{"content":"explore"}}"#,
    )?;
    let read = read(&claude(dir.path()), HarnessId::ClaudeCode, "a1").await;
    assert_eq!(
        serde_json::to_string(&read.turns)?,
        r#"[{"role":"user","text":"explore"}]"#
    );
    Ok(())
}

#[tokio::test]
async fn b5_11_a_missing_store_file_is_an_empty_transcript() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        &format!("claude/projects/-home-user/{U1}.jsonl"),
        &user("claude only"),
    )?;
    let lister = StoreSessionLister::new(StoreRoots {
        claude: Some(dir.path().join("claude")),
        hermes: Some(dir.path().join("hermes")),
        ..StoreRoots::default()
    });
    assert_eq!(read(&lister, HarnessId::Hermes, U1).await.turns, Vec::new());
    assert_eq!(
        read(
            &lister,
            HarnessId::ClaudeCode,
            "00000000-0000-4000-8000-000000000002"
        )
        .await
        .turns,
        Vec::new()
    );
    assert_eq!(read(&lister, HarnessId::Codex, U1).await.turns, Vec::new());
    assert_eq!(
        read(&lister, HarnessId::ClaudeCode, U1).await.turns.len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn cowork_has_no_transcript_source() -> TestResult {
    let lister = StoreSessionLister::new(StoreRoots::default());
    assert_eq!(
        lister
            .transcript(HarnessId::Cowork, "c1", TranscriptWindow::Latest)
            .await?,
        TranscriptRead::NoSource
    );
    Ok(())
}

#[tokio::test]
async fn other_harnesses_read_text_turns_from_their_stores() -> TestResult {
    let dir = tempfile::tempdir()?;
    let session = dir.path().join(format!(
        "sessions/-work-demo/2026-10-14T12-00-00-000Z_{OMP}.jsonl"
    ));
    fs::create_dir_all(session.parent().ok_or("parent")?)?;
    fs::copy(testkit::recording("capture/stores/omp.jsonl"), &session)?;
    let lister = StoreSessionLister::new(StoreRoots {
        omp: Some(dir.path().to_path_buf()),
        ..StoreRoots::default()
    });
    let read = read(&lister, HarnessId::Omp, OMP).await;
    assert_eq!(
        serde_json::to_string(&read.turns)?,
        r#"[{"role":"user","text":"list the files","startedAtMs":1791979200000,"endedAtMs":1791979200000},{"role":"assistant","text":"Running ls.","startedAtMs":1791979201000,"endedAtMs":1791979201000}]"#
    );
    Ok(())
}

fn image_turn(text: Option<&str>) -> String {
    let lead = text.map_or_else(String::new, |text| {
        format!(
            r#"{{"type":"text","text":{}}},"#,
            serde_json::to_string(text).expect("text")
        )
    });
    format!(
        r#"{{"type":"user","message":{{"role":"user","content":[{lead}{{"type":"image","source":{{"type":"base64","media_type":"image/png","data":"iVBORw0K"}}}}]}}}}"#
    )
}

#[tokio::test]
async fn a_user_turn_carries_its_image_blocks_as_attachments() {
    let meta = r#"{"type":"user","isMeta":true,"message":{"role":"user","content":[{"type":"text","text":"[Image: source: /tmp/images/1.png]"}]}}"#;
    let done = assistant(r#"[{"type":"text","text":"Red"}]"#, "end_turn");
    assert_eq!(
        claude_json(&[&image_turn(Some("what color?")), meta, &done]).await,
        r#"[{"role":"user","text":"what color?","attachments":[{"mediaType":"image/png"}]},{"role":"assistant","text":"Red","stopReason":"end_turn","lastBlock":"text","complete":true}]"#
    );
}

#[tokio::test]
async fn an_image_only_user_turn_is_kept_with_empty_text() {
    let done = assistant(r#"[{"type":"text","text":"Red"}]"#, "end_turn");
    assert_eq!(
        claude_json(&[&image_turn(None), &done]).await,
        r#"[{"role":"user","text":"","attachments":[{"mediaType":"image/png"}]},{"role":"assistant","text":"Red","stopReason":"end_turn","lastBlock":"text","complete":true}]"#
    );
}

#[tokio::test]
async fn a_turn_spans_its_first_and_last_store_timestamps() {
    let stamped = |line: &str, at: &str| format!(r#"{{"timestamp":"{at}",{}"#, &line[1..]);
    let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"a.txt"}]}}"#;
    let done = assistant(r#"[{"type":"text","text":"done"}]"#, "end_turn");
    let turns = claude_turns(&[
        &stamped(&user("list"), "2026-10-14T12:00:00Z"),
        &stamped(
            &tool_use("t1", "Bash", r#"{"command":"ls"}"#),
            "2026-10-14T12:00:01Z",
        ),
        &stamped(result, "2026-10-14T12:00:03Z"),
        &stamped(&done, "2026-10-14T12:00:04.500Z"),
    ])
    .await;
    let times: Vec<_> = turns
        .iter()
        .map(|turn| (turn.started_at_ms, turn.ended_at_ms))
        .collect();
    assert_eq!(
        times,
        [
            (Some(1_791_979_200_000), Some(1_791_979_200_000)),
            (Some(1_791_979_201_000), Some(1_791_979_204_500)),
        ]
    );
}
