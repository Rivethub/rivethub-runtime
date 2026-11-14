use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use parking_lot::Mutex;
use testkit::Json;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::CaptureResult;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SESSION: &str = "8d10b957-6b03-4e97-bab1-b9f41ef0b517";
const SUBAGENT: &str = "a634b960ec50d3ecb";
const AGENT_CALL: &str = "call_cc028e053174460ab33f9a86";
const AGENT_LINE: usize = 10;

#[derive(Clone, Default)]
struct FirstWriteWins {
    rows: Arc<Mutex<BTreeMap<(String, String), CaptureMessage>>>,
    posts: Arc<Mutex<usize>>,
}

#[async_trait]
impl capture::writer::CaptureTransport for FirstWriteWins {
    async fn post(&self, body: String) -> Result<CaptureResult, capture::writer::PostError> {
        let batch: CaptureBatchRequest =
            serde_json::from_str(&body).map_err(|error| capture::writer::PostError::Network {
                message: error.to_string(),
            })?;
        *self.posts.lock() += 1;
        let mut rows = self.rows.lock();
        let mut inserted = 0;
        for message in batch.messages {
            let key = (batch.session_key.clone(), message.event_id.clone());
            if let std::collections::btree_map::Entry::Vacant(slot) = rows.entry(key) {
                slot.insert(message);
                inserted += 1;
            }
        }
        Ok(CaptureResult {
            ok: true,
            conversation_id: "conv-1".to_owned(),
            inserted,
            skipped: 0,
        })
    }
}

impl FirstWriteWins {
    fn row(&self, event_id: &str) -> Option<CaptureMessage> {
        self.rows
            .lock()
            .iter()
            .find(|((_, id), _)| id == event_id)
            .map(|(_, message)| message.clone())
    }

    fn contents(&self) -> Vec<(Role, String)> {
        self.rows
            .lock()
            .values()
            .map(|message| (message.role, message.content.clone()))
            .collect()
    }
}

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Result<Self, std::io::Error> {
        let home = Self {
            dir: tempfile::tempdir()?,
        };
        std::fs::create_dir_all(
            home.transcript()
                .parent()
                .ok_or_else(|| std::io::Error::other("transcript path has no parent"))?,
        )?;
        Ok(home)
    }

    fn transcript(&self) -> PathBuf {
        self.dir
            .path()
            .join(".claude/projects/-home-user-acmekit")
            .join(format!("{SESSION}.jsonl"))
    }

    fn write(&self, lines: &[String]) -> Result<(), std::io::Error> {
        std::fs::write(self.transcript(), lines.concat())
    }

    fn stdin(&self, event: &str, extra: &str) -> String {
        format!(
            r#"{{"session_id":"{SESSION}","transcript_path":"{}","cwd":"/home/user/acmekit","hook_event_name":"{event}"{extra}}}"#,
            self.transcript().display()
        )
    }

    async fn run(&self, gateway: &FirstWriteWins, stdin: &str) -> TestResult {
        let env = config::Environment::from_vars([(
            "HOME".to_owned(),
            self.dir.path().display().to_string(),
        )]);
        let hook = capture::hook::ClaudeHook::new(
            capture::writer::CaptureWriter::new(
                gateway.clone(),
                capture::spool::CaptureSpool::new(self.dir.path().join("spool")),
                None,
                None,
            ),
            &config::Paths::new(&env)?,
        );
        let now: DateTime<Utc> = "2026-11-14T18:16:00Z".parse()?;
        let run = hook.run(stdin, now, "1794679000000-0000000001").await;
        assert_eq!(run.stderr, "");
        Ok(())
    }
}

fn parent_lines() -> Result<Vec<String>, std::io::Error> {
    Ok(
        std::fs::read_to_string(testkit::recording("capture/claude-subagent/parent.jsonl"))?
            .lines()
            .map(|line| format!("{line}\n"))
            .collect(),
    )
}

fn event(id: &str) -> String {
    format!("claude-code:{SESSION}:tool:{id}")
}

#[tokio::test]
async fn the_agent_row_keeps_the_subagents_answer_when_subagent_stop_comes_first() -> TestResult {
    let home = Home::new()?;
    let lines = parent_lines()?;
    let gateway = FirstWriteWins::default();
    home.write(lines.get(..AGENT_LINE).ok_or("fixture is short")?)?;
    home.run(
        &gateway,
        &home.stdin("SubagentStop", &format!(r#","agent_id":"{SUBAGENT}""#)),
    )
    .await?;
    home.write(&lines)?;
    home.run(&gateway, &home.stdin("Stop", "")).await?;
    let agent = gateway.row(&event(AGENT_CALL)).ok_or("agent row")?;
    assert_eq!(agent.tool_name.as_deref(), Some("Agent"));
    let result = agent.tool_result.ok_or("the agent row has no result")?;
    assert!(
        result.starts_with("[Subagent hand-back]") && result.contains("196"),
        "{result}"
    );
    Ok(())
}

#[tokio::test]
async fn a_session_end_still_keeps_a_call_whose_result_never_came() -> TestResult {
    let home = Home::new()?;
    let lines = parent_lines()?;
    let gateway = FirstWriteWins::default();
    home.write(lines.get(..AGENT_LINE).ok_or("fixture is short")?)?;
    home.run(&gateway, &home.stdin("SessionEnd", "")).await?;
    let agent = gateway.row(&event(AGENT_CALL)).ok_or("agent row")?;
    assert_eq!(agent.content, "[tool call] Agent");
    assert_eq!(agent.tool_result, None);
    Ok(())
}

#[tokio::test]
async fn a_tool_call_made_inside_a_subagent_is_not_written_into_the_parent() -> TestResult {
    let home = Home::new()?;
    home.write(&parent_lines()?)?;
    let mut payload: Json = serde_json::from_str(&std::fs::read_to_string(testkit::recording(
        "capture/claude-subagent/post-tool-use-in-subagent.json",
    ))?)?;
    let object = payload.as_object_mut().ok_or("payload")?;
    object.insert("session_id".to_owned(), Json::from(SESSION));
    object.insert(
        "transcript_path".to_owned(),
        Json::from(home.transcript().display().to_string()),
    );
    let gateway = FirstWriteWins::default();
    home.run(&gateway, &payload.to_string()).await?;
    assert_eq!(*gateway.posts.lock(), 0);
    assert_eq!(gateway.contents(), Vec::new());
    Ok(())
}

#[tokio::test]
async fn a_thinking_block_whose_text_was_omitted_stores_no_row() -> TestResult {
    let home = Home::new()?;
    let mut lines = parent_lines()?;
    let thinking = lines.get_mut(1).ok_or("fixture is short")?;
    let mut line: Json = serde_json::from_str(thinking)?;
    let block = line
        .pointer_mut("/message/content/0")
        .and_then(Json::as_object_mut)
        .ok_or("thinking block")?;
    assert_eq!(block.get("type"), Some(&Json::from("thinking")));
    block.insert("thinking".to_owned(), Json::from(""));
    *thinking = format!("{line}\n");
    home.write(&lines)?;
    let gateway = FirstWriteWins::default();
    home.run(&gateway, &home.stdin("Stop", "")).await?;
    let thoughts: Vec<String> = gateway
        .contents()
        .into_iter()
        .filter(|(_, content)| content.starts_with("[thinking]"))
        .map(|(_, content)| content)
        .collect();
    assert_eq!(thoughts.len(), 2, "{thoughts:?}");
    assert!(
        thoughts
            .iter()
            .all(|content| content.len() > "[thinking] ".len()),
        "{thoughts:?}"
    );
    Ok(())
}
