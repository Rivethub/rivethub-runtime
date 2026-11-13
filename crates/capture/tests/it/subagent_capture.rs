use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use parking_lot::Mutex;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureResult;
use wire::harness::HarnessId;

const PARENT: &str = "55555555-5555-5555-5555-555555555555";
const EXPLORER: &str = "a1b2c3d4e5f60718a";
const WORKFLOW: &str = "a99887766554433ff";

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Clone, Default)]
struct Gateway {
    seen: Arc<Mutex<HashSet<String>>>,
    batches: Arc<Mutex<Vec<CaptureBatchRequest>>>,
}

#[async_trait]
impl capture::writer::CaptureTransport for Gateway {
    async fn post(&self, body: String) -> Result<CaptureResult, capture::writer::PostError> {
        let batch: CaptureBatchRequest =
            serde_json::from_str(&body).map_err(|error| capture::writer::PostError::Network {
                message: error.to_string(),
            })?;
        let mut seen = self.seen.lock();
        let inserted = batch
            .messages
            .iter()
            .filter(|message| seen.insert(message.event_id.clone()))
            .count();
        self.batches.lock().push(batch);
        Ok(CaptureResult {
            ok: true,
            conversation_id: "conv-1".to_owned(),
            inserted: inserted as u64,
            skipped: 0,
        })
    }
}

impl Gateway {
    fn keys(&self) -> Vec<String> {
        self.batches
            .lock()
            .iter()
            .map(|batch| batch.session_key.clone())
            .collect()
    }

    fn take(&self) -> Vec<CaptureBatchRequest> {
        std::mem::take(&mut *self.batches.lock())
    }
}

fn line(kind: &str, uuid: &str, text: &str, agent: Option<&str>) -> String {
    let content = if kind == "user" {
        format!("\"{text}\"")
    } else {
        format!(r#"[{{"type":"text","text":"{text}"}}]"#)
    };
    let sidechain = agent.map_or(String::new(), |id| {
        format!(r#""isSidechain":true,"agentId":"{id}","#)
    });
    format!(
        r#"{{{sidechain}"type":"{kind}","uuid":"{uuid}","cwd":"/work/demo","timestamp":"2026-10-10T12:00:00.000Z","message":{{"role":"{kind}","content":{content}}}}}"#
    )
}

struct Store {
    home: tempfile::TempDir,
}

impl Store {
    fn new() -> Result<Self, std::io::Error> {
        let home = tempfile::tempdir()?;
        let store = Self { home };
        std::fs::create_dir_all(store.session_dir())?;
        std::fs::write(
            store.parent_transcript(),
            format!(
                "{}\n{}\n",
                line("user", "p1", "split the work", None),
                line("assistant", "p2", "spawning helpers", None)
            ),
        )?;
        Ok(store)
    }

    fn slug(&self) -> PathBuf {
        self.home.path().join(".claude/projects/-work-demo")
    }

    fn session_dir(&self) -> PathBuf {
        self.slug().join(PARENT)
    }

    fn parent_transcript(&self) -> PathBuf {
        self.slug().join(format!("{PARENT}.jsonl"))
    }

    fn agent(directory: &Path, id: &str, meta: &str) -> Result<PathBuf, std::io::Error> {
        std::fs::create_dir_all(directory)?;
        std::fs::write(directory.join(format!("agent-{id}.meta.json")), meta)?;
        let path = directory.join(format!("agent-{id}.jsonl"));
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                line("user", "c1", "find the widget", Some(id)),
                line("assistant", "c2", "found the widget", Some(id))
            ),
        )?;
        Ok(path)
    }

    fn explorer(&self) -> Result<PathBuf, std::io::Error> {
        Self::agent(
            &self.session_dir().join("subagents"),
            EXPLORER,
            r#"{"agentType":"Explore","description":"Locate the widget","toolUseId":"toolu_01"}"#,
        )
    }

    fn workflow_agent(&self) -> Result<PathBuf, std::io::Error> {
        Self::agent(
            &self.session_dir().join("subagents/workflows/wf_1"),
            WORKFLOW,
            r#"{"agentType":"general-purpose","description":"Fan out","toolUseId":"toolu_02"}"#,
        )
    }

    fn hook(
        &self,
        gateway: &Gateway,
    ) -> Result<capture::hook::ClaudeHook<Gateway>, config::ConfigError> {
        let env = environment(self.home.path());
        Ok(capture::hook::ClaudeHook::new(
            writer(self.home.path(), gateway),
            &config::Paths::new(&env)?,
        ))
    }

    fn stdin(&self, event: &str, extra: &str) -> String {
        format!(
            r#"{{"session_id":"{PARENT}","transcript_path":"{}","cwd":"/work/demo","hook_event_name":"{event}"{extra}}}"#,
            self.parent_transcript().display()
        )
    }
}

fn environment(home: &Path) -> config::Environment {
    config::Environment::from_vars([("HOME".to_owned(), home.display().to_string())])
}

fn writer(home: &Path, gateway: &Gateway) -> capture::writer::CaptureWriter<Gateway> {
    capture::writer::CaptureWriter::new(
        gateway.clone(),
        capture::spool::CaptureSpool::new(home.join("spool")),
        None,
        None,
    )
}

fn now() -> Result<DateTime<Utc>, chrono::ParseError> {
    "2026-10-10T12:05:00Z".parse()
}

fn the<'a>(
    batches: &'a [CaptureBatchRequest],
    key: &str,
) -> Result<&'a CaptureBatchRequest, String> {
    batches
        .iter()
        .find(|batch| batch.session_key == key)
        .ok_or_else(|| format!("no batch was posted for {key}"))
}

fn setting<'a>(batch: &'a CaptureBatchRequest, name: &str) -> Option<&'a str> {
    batch.settings.as_ref()?.get(name)?.as_str()
}

#[tokio::test]
async fn a_subagent_stop_captures_the_subagents_own_transcript() -> TestResult {
    let store = Store::new()?;
    store.explorer()?;
    let gateway = Gateway::default();
    let run = store
        .hook(&gateway)?
        .run(
            &store.stdin("SubagentStop", &format!(r#","agent_id":"{EXPLORER}""#)),
            now()?,
            "1791656243919-30e4c5d70",
        )
        .await;
    assert_eq!((run.exit, run.stderr.as_str()), (0, ""));
    let batches = gateway.take();
    let child = the(&batches, &format!("claude-code:{EXPLORER}"))?;
    assert_eq!(child.title.as_deref(), Some("Locate the widget"));
    assert_eq!(setting(child, "parent_session_id"), Some(PARENT));
    assert_eq!(setting(child, "agent_id"), Some(EXPLORER));
    assert_eq!(setting(child, "agent_type"), Some("Explore"));
    assert_eq!(setting(child, "tool_use_id"), Some("toolu_01"));
    assert_eq!(child.finalize, Some(true));
    let rows: Vec<&str> = child
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect();
    assert_eq!(rows, ["find the widget", "found the widget"]);
    let prefix = format!("claude-code:{EXPLORER}:");
    assert!(
        child
            .messages
            .iter()
            .all(|message| message.event_id.starts_with(&prefix))
    );
    let parent = the(&batches, &format!("claude-code:{PARENT}"))?;
    assert_eq!(setting(parent, "parent_session_id"), None);
    Ok(())
}

#[tokio::test]
async fn a_stop_catches_up_subagents_it_has_not_captured_and_then_leaves_them() -> TestResult {
    let store = Store::new()?;
    let explorer = store.explorer()?;
    store.workflow_agent()?;
    let gateway = Gateway::default();
    let hook = store.hook(&gateway)?;
    let stop = store.stdin("Stop", "");
    hook.run(&stop, now()?, "1791656243919-30e4c5d70").await;
    let mut keys = gateway.keys();
    keys.sort();
    assert_eq!(
        keys,
        [
            format!("claude-code:{PARENT}"),
            format!("claude-code:{EXPLORER}"),
            format!("claude-code:{WORKFLOW}"),
        ]
    );
    gateway.take();

    hook.run(&stop, now()?, "1791656243919-30e4c5d71").await;
    assert_eq!(gateway.keys(), [format!("claude-code:{PARENT}")]);
    gateway.take();

    let mut grown = std::fs::read_to_string(&explorer)?;
    grown.push_str(&line(
        "assistant",
        "c3",
        "and one more thing",
        Some(EXPLORER),
    ));
    grown.push('\n');
    std::fs::write(&explorer, grown)?;
    hook.run(&stop, now()?, "1791656243919-30e4c5d72").await;
    let mut keys = gateway.keys();
    keys.sort();
    assert_eq!(
        keys,
        [
            format!("claude-code:{PARENT}"),
            format!("claude-code:{EXPLORER}"),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn backfill_captures_parents_and_subagents_and_a_second_run_adds_nothing() -> TestResult {
    let store = Store::new()?;
    store.explorer()?;
    store.workflow_agent()?;
    let gateway = Gateway::default();
    let backfill = capture::backfill::Backfill::new(
        writer(store.home.path(), &gateway),
        capture::listing::store_roots(&environment(store.home.path())),
    );
    let ids = [HarnessId::ClaudeCode];

    let first = backfill.run(&ids, false, now()?).await;
    let totals = first.first().ok_or("no totals")?;
    assert_eq!(
        (
            totals.sessions,
            totals.messages,
            totals.added,
            totals.failures.len()
        ),
        (3, 6, 6, 0)
    );
    let batches = gateway.take();
    for key in [PARENT, EXPLORER, WORKFLOW] {
        the(&batches, &format!("claude-code:{key}"))?;
    }
    let workflow = the(&batches, &format!("claude-code:{WORKFLOW}"))?;
    assert_eq!(setting(workflow, "parent_session_id"), Some(PARENT));
    assert_eq!(workflow.finalize, None);

    let second = backfill.run(&ids, false, now()?).await;
    let totals = second.first().ok_or("no totals")?;
    assert_eq!(
        (
            totals.sessions,
            totals.messages,
            totals.added,
            totals.failures.len()
        ),
        (3, 6, 0, 0)
    );
    Ok(())
}

#[tokio::test]
async fn backfill_writes_the_event_ids_the_hook_wrote() -> TestResult {
    let store = Store::new()?;
    store.explorer()?;
    let gateway = Gateway::default();
    store
        .hook(&gateway)?
        .run(&store.stdin("Stop", ""), now()?, "1791656243919-30e4c5d70")
        .await;
    let backfill = capture::backfill::Backfill::new(
        writer(store.home.path(), &gateway),
        capture::listing::store_roots(&environment(store.home.path())),
    );
    let found = backfill.run(&[HarnessId::ClaudeCode], false, now()?).await;
    let totals = found.first().ok_or("no totals")?;
    assert_eq!((totals.sessions, totals.added), (2, 0));
    Ok(())
}

#[derive(Clone, Default)]
struct Down;

#[async_trait]
impl capture::writer::CaptureTransport for Down {
    async fn post(&self, _body: String) -> Result<CaptureResult, capture::writer::PostError> {
        Err(capture::writer::PostError::Network {
            message: "connection refused".to_owned(),
        })
    }
}

fn spooled_keys(home: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut keys = Vec::new();
    for entry in std::fs::read_dir(home.join("spool"))? {
        let path = entry?.path();
        if path.is_file() {
            let batch: CaptureBatchRequest = serde_json::from_slice(&std::fs::read(&path)?)?;
            keys.push(batch.session_key);
        }
    }
    keys.sort();
    Ok(keys)
}

#[tokio::test]
async fn a_subagent_stop_during_an_outage_spools_the_subagent_as_well_as_the_parent() -> TestResult
{
    let store = Store::new()?;
    store.explorer()?;
    let env = environment(store.home.path());
    let hook = capture::hook::ClaudeHook::new(
        capture::writer::CaptureWriter::new(
            Down,
            capture::spool::CaptureSpool::new(store.home.path().join("spool")),
            None,
            None,
        ),
        &config::Paths::new(&env)?,
    );
    hook.run(
        &store.stdin("SubagentStop", &format!(r#","agent_id":"{EXPLORER}""#)),
        now()?,
        "1791656243919-30e4c5d71",
    )
    .await;
    assert_eq!(
        spooled_keys(store.home.path())?,
        vec![
            format!("claude-code:{PARENT}"),
            format!("claude-code:{EXPLORER}")
        ]
    );
    Ok(())
}

fn down_hook(
    store: &Store,
    spool: PathBuf,
) -> Result<capture::hook::ClaudeHook<Down>, config::ConfigError> {
    let env = environment(store.home.path());
    Ok(capture::hook::ClaudeHook::new(
        capture::writer::CaptureWriter::new(
            Down,
            capture::spool::CaptureSpool::new(spool),
            None,
            None,
        ),
        &config::Paths::new(&env)?,
    ))
}

#[tokio::test]
async fn repeated_stops_during_an_outage_spool_a_subagent_once() -> TestResult {
    let store = Store::new()?;
    store.explorer()?;
    let hook = down_hook(&store, store.home.path().join("spool"))?;
    for key in ["1791656243919-30e4c5d71", "1791656243920-30e4c5d72"] {
        let run = hook.run(&store.stdin("Stop", ""), now()?, key).await;
        assert_eq!(run.exit, 0);
    }
    let keys = spooled_keys(store.home.path())?;
    let parent = format!("claude-code:{PARENT}");
    let subagent = format!("claude-code:{EXPLORER}");
    assert_eq!(keys, vec![parent.clone(), parent, subagent]);
    Ok(())
}

#[tokio::test]
async fn a_stop_that_cannot_save_the_parent_or_a_subagent_exits_1() -> TestResult {
    let store = Store::new()?;
    store.explorer()?;
    let blocked = store.home.path().join("spool");
    std::fs::write(&blocked, "not a directory")?;
    let hook = down_hook(&store, blocked)?;
    let run = hook
        .run(&store.stdin("Stop", ""), now()?, "1791656243919-30e4c5d71")
        .await;
    assert_eq!(run.exit, 1, "{}", run.stderr);
    assert_eq!(run.stderr.matches("capture batch was NOT saved").count(), 2);
    Ok(())
}

#[tokio::test]
async fn backfill_reads_every_codex_session() -> TestResult {
    let home = tempfile::tempdir()?;
    let day = home.path().join(".codex/sessions/2026/10/30");
    std::fs::create_dir_all(&day)?;
    for id in [
        "01a18e31-2073-7cc2-be8a-8e36c42f8948",
        "01a18e31-2073-7cc2-be8a-8e36c42f8949",
    ] {
        std::fs::write(
            day.join(format!("rollout-2026-10-30T15-00-00-{id}.jsonl")),
            format!(
                r#"{{"timestamp":"2026-10-30T15:00:00.000Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/work/codex"}}}}
{{"timestamp":"2026-10-30T15:00:03.000Z","type":"response_item","payload":{{"type":"message","role":"user","id":"msg_user","content":[{{"type":"input_text","text":"apply the patch"}}]}}}}
{{"timestamp":"2026-10-30T15:00:10.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Applied."}}]}}}}
"#
            ),
        )?;
    }
    let gateway = Gateway::default();
    let totals = capture::backfill::Backfill::new(
        writer(home.path(), &gateway),
        capture::listing::store_roots(&environment(home.path())),
    )
    .run(&[HarnessId::Codex], false, now()?)
    .await;
    let codex = totals.first().ok_or("no totals")?;
    assert_eq!((codex.sessions, codex.failures.len()), (2, 0));
    assert!(codex.added > 0, "backfill added no codex messages");
    Ok(())
}
