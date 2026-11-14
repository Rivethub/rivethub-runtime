use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::Connection;
use serde::Deserialize;
use testkit::Json;
use wire::capture::CaptureResult;

use capture::hook::store::StoreHarness;
use capture::hook::store::StoreHook;
use capture::listing::StoreRoots;
use capture::listing::StoreSessionLister;
use capture::listing::store_roots;
use capture::spool::CaptureSpool;
use capture::writer::CaptureTransport;
use capture::writer::CaptureWriter;
use capture::writer::PostError;
use kernel::transcript::SessionLister;
use wire::harness::HarnessId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const PARENT: &str = "ses_e24d7a71affeqgtW54E9z7nTL2";
const CHILD: &str = "ses_e24d73a33ffe4fw19RfAK47bv8";
const NOW_TEXT: &str = "2026-11-14T17:45:00.000Z";

const SCHEMA: &str = "CREATE TABLE `session_v2` (
  `id` text PRIMARY KEY, `project_id` text NOT NULL, `workspace_id` text, `parent_id` text,
  `fork_session_id` text, `fork_boundary` text, `slug` text NOT NULL, `directory` text NOT NULL,
  `path` text, `title` text, `version` text NOT NULL, `share_url` text, `summary_additions` integer,
  `summary_deletions` integer, `summary_files` integer, `summary_diffs` text, `metadata` text,
  `cost` real DEFAULT 0 NOT NULL, `tokens_input` integer DEFAULT 0 NOT NULL,
  `tokens_output` integer DEFAULT 0 NOT NULL, `tokens_reasoning` integer DEFAULT 0 NOT NULL,
  `tokens_cache_read` integer DEFAULT 0 NOT NULL, `tokens_cache_write` integer DEFAULT 0 NOT NULL,
  `revert` text, `permission` text, `agent` text, `model` text, `time_created` integer NOT NULL,
  `time_updated` integer NOT NULL, `time_busy` integer, `time_idle` integer, `time_viewed` integer,
  `idle_outcome` text, `time_compacting` integer, `time_archived` integer, `time_suspended` integer,
  `resume_attempts` integer DEFAULT 0 NOT NULL
);
CREATE INDEX `session_v2_parent_idx` ON `session_v2` (`parent_id`);
CREATE TABLE `session_message` (
  `id` text PRIMARY KEY, `session_id` text NOT NULL, `type` text NOT NULL, `seq` integer NOT NULL,
  `time_created` integer NOT NULL, `time_updated` integer NOT NULL, `data` text NOT NULL
);
CREATE UNIQUE INDEX `session_message_session_seq_idx` ON `session_message` (`session_id`,`seq`);
CREATE TABLE `session_part` (
  `message_id` text NOT NULL, `session_id` text NOT NULL, `ordinal` integer NOT NULL,
  `type` text NOT NULL, `call_id` text, `time_created` integer NOT NULL,
  `time_updated` integer NOT NULL, `data` text NOT NULL,
  CONSTRAINT `session_part_pk` PRIMARY KEY(`message_id`, `ordinal`)
);";

const SESSION_INSERT: &str = "INSERT INTO session_v2 (id, parent_id, project_id, slug, directory, title, version, agent, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";
const MESSAGE_INSERT: &str = "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
const PART_INSERT: &str = "INSERT INTO session_part (message_id, session_id, ordinal, type, call_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";
const SUBAGENT_FIXTURE: &str = "capture/stores/opencode-v2-subagent.jsonl";
const PART_ROWS_FIXTURE: &str = "capture/stores/opencode-v2-part-rows.jsonl";
const PART_ROWS_SESSION: &str = "ses_e249b94edffeT8KcrgxiLPPm7c";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    AsRecorded,
    PartRows,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "table")]
enum StoreRow {
    #[serde(rename = "session_v2")]
    Session {
        id: String,
        parent_id: Option<String>,
        project_id: String,
        slug: String,
        directory: String,
        title: String,
        version: String,
        agent: Option<String>,
        time_created: i64,
        time_updated: i64,
    },
    #[serde(rename = "session_message")]
    Message {
        id: String,
        session_id: String,
        #[serde(rename = "type")]
        kind: String,
        seq: i64,
        time_created: i64,
        time_updated: i64,
        data: String,
    },
    #[serde(rename = "session_part")]
    Part {
        message_id: String,
        session_id: String,
        ordinal: i64,
        #[serde(rename = "type")]
        kind: String,
        call_id: Option<String>,
        time_created: i64,
        time_updated: i64,
        data: String,
    },
}

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

fn opencode_2_3_store(home: &Path) -> TestResult {
    opencode_2_3_store_from(home, SUBAGENT_FIXTURE, Layout::AsRecorded)
}

fn into_part_rows(
    connection: &Connection,
    id: &str,
    session_id: &str,
    time_created: i64,
    data: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut message: Json = serde_json::from_str(data)?;
    let items = message
        .get_mut("content")
        .and_then(Json::as_array_mut)
        .map(std::mem::take)
        .ok_or("assistant content")?;
    for (ordinal, item) in items.into_iter().enumerate() {
        let kind = item
            .get("type")
            .and_then(Json::as_str)
            .ok_or("item type")?
            .to_owned();
        let call_id = item.get("id").and_then(Json::as_str).map(str::to_owned);
        connection.execute(
            PART_INSERT,
            (
                id,
                session_id,
                i64::try_from(ordinal)?,
                kind,
                call_id,
                time_created,
                time_created,
                item.to_string(),
            ),
        )?;
    }
    Ok(message.to_string())
}

fn opencode_2_3_store_from(home: &Path, fixture: &str, layout: Layout) -> TestResult {
    let data = home.join(".local/share/opencode");
    std::fs::create_dir_all(&data)?;
    let connection = Connection::open(data.join("opencode.db"))?;
    connection.execute_batch(SCHEMA)?;
    let rows = std::fs::read_to_string(testkit::recording(fixture))?;
    for line in rows.lines() {
        match serde_json::from_str::<StoreRow>(line)? {
            StoreRow::Session {
                id,
                parent_id,
                project_id,
                slug,
                directory,
                title,
                version,
                agent,
                time_created,
                time_updated,
            } => connection.execute(
                SESSION_INSERT,
                (
                    id,
                    parent_id,
                    project_id,
                    slug,
                    directory,
                    title,
                    version,
                    agent,
                    time_created,
                    time_updated,
                ),
            )?,
            StoreRow::Message {
                id,
                session_id,
                kind,
                seq,
                time_created,
                time_updated,
                data,
            } => {
                let data = if layout == Layout::PartRows && kind == "assistant" {
                    into_part_rows(&connection, &id, &session_id, time_created, &data)?
                } else {
                    data
                };
                connection.execute(
                    MESSAGE_INSERT,
                    (id, session_id, kind, seq, time_created, time_updated, data),
                )?
            }
            StoreRow::Part {
                message_id,
                session_id,
                ordinal,
                kind,
                call_id,
                time_created,
                time_updated,
                data,
            } => connection.execute(
                PART_INSERT,
                (
                    message_id,
                    session_id,
                    ordinal,
                    kind,
                    call_id,
                    time_created,
                    time_updated,
                    data,
                ),
            )?,
        };
    }
    Ok(())
}

async fn idle_batch(session: &str) -> Result<Json, Box<dyn std::error::Error>> {
    idle_batch_from(SUBAGENT_FIXTURE, Layout::AsRecorded, session).await
}

async fn idle_batch_from(
    fixture: &str,
    layout: Layout,
    session: &str,
) -> Result<Json, Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    opencode_2_3_store_from(home.path(), fixture, layout)?;
    let env =
        config::Environment::from_vars([("HOME".to_owned(), home.path().display().to_string())]);
    let transport = FakeTransport::default();
    let writer = CaptureWriter::new(
        transport.clone(),
        CaptureSpool::new(home.path().join("spool")),
        None,
        None,
    );
    let hook = StoreHook::new(
        StoreHarness::Opencode,
        writer,
        &store_roots(&env),
        &config::Paths::new(&env)?,
    )
    .ok_or("opencode store")?;
    let now: DateTime<Utc> = NOW_TEXT.parse()?;
    let payload = format!(
        r#"{{"session_id":"{session}","hook_event_name":"session.idle","cwd":"/home/user/acmekit"}}"#
    );
    assert_eq!(hook.run(&payload, now).await.stderr, "");
    let posts = transport.posts.lock().clone();
    let [body] = posts.as_slice() else {
        return Err(format!("expected one post, got {}", posts.len()).into());
    };
    Ok(serde_json::from_str(body)?)
}

fn at<'a>(batch: &'a Json, pointer: &str) -> &'a Json {
    batch.pointer(pointer).unwrap_or(&Json::Null)
}

fn rows(batch: &Json) -> Vec<(String, String, String)> {
    at(batch, "/messages")
        .as_array()
        .into_iter()
        .flatten()
        .map(|message| {
            let text = |pointer| at(message, pointer).as_str().unwrap_or("-").to_owned();
            let content = text("/content");
            (
                text("/role"),
                text("/tool_name"),
                content.chars().take(60).collect(),
            )
        })
        .collect()
}

fn row(role: &str, tool: &str, content: &str) -> (String, String, String) {
    (
        role.to_owned(),
        tool.to_owned(),
        content.chars().take(60).collect(),
    )
}

#[tokio::test]
async fn opencode_2_3_reasoning_becomes_thinking_rows_in_order() -> TestResult {
    let batch = idle_batch(PARENT).await?;
    assert_eq!(
        rows(&batch),
        vec![
            row(
                "user",
                "-",
                "Do these steps in order. 1) Run the shell command: uname -sr"
            ),
            row(
                "assistant",
                "-",
                "[thinking] The user wants me to execute three distinct steps: f"
            ),
            row("tool", "shell", "Linux 6.8.0-acme\n"),
            row(
                "tool",
                "read",
                "Read file notes.txt, lines 1-1\n1: marker: PELICAN-42"
            ),
            row(
                "tool",
                "subagent",
                "The subagent is working in the background (sessionID: ses_e2"
            ),
            row(
                "assistant",
                "-",
                "[thinking] I've got the kernel version and marker down, so now "
            ),
            row(
                "assistant",
                "-",
                "Steps 1 and 2 are done (kernel: Linux 6.8.0-acme, marker: PEL"
            ),
            row(
                "system",
                "-",
                "<subagent sessionID=\"ses_e24d73a33ffe4fw19RfAK47bv8\" state=\""
            ),
            row(
                "assistant",
                "-",
                "[thinking] The user is asking for a one-liner that includes th"
            ),
            row("assistant", "-", "Linux 6.8.0-acme | PELICAN-42 | 196"),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn opencode_2_3_subagent_stores_no_blank_assistant_row() -> TestResult {
    let batch = idle_batch(CHILD).await?;
    assert_eq!(
        rows(&batch),
        vec![
            row(
                "user",
                "-",
                "You are a subagent spawned by another session.\nRun a shell c"
            ),
            row(
                "assistant",
                "-",
                "[thinking] The user is asking to run a shell command to count t"
            ),
            row("tool", "shell", "196\n"),
            row(
                "assistant",
                "-",
                "[thinking] [Thinking] The user wants only the number reported."
            ),
            row("assistant", "-", "196"),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn opencode_2_3_subagent_settings_name_the_parent_like_claude_code() -> TestResult {
    let child = idle_batch(CHILD).await?;
    assert_eq!(at(&child, "/settings/parent_session_id"), PARENT);
    assert_eq!(at(&child, "/settings/agent_id"), CHILD);
    assert_eq!(at(&child, "/settings/agent_type"), "explore");
    assert_eq!(at(&child, "/settings/description"), "Count entries in /etc");
    assert_eq!(
        at(&child, "/settings/tool_use_id"),
        "call_5d7c0668d51048c8aa5d86e4"
    );
    let parent = idle_batch(PARENT).await?;
    let settings = at(&parent, "/settings").as_object().ok_or("settings")?;
    for key in [
        "parent_session_id",
        "agent_id",
        "agent_type",
        "description",
        "tool_use_id",
    ] {
        assert!(!settings.contains_key(key), "{key} in {settings:?}");
    }
    Ok(())
}

#[tokio::test]
async fn opencode_2_3_listing_names_the_subagent_parent() -> TestResult {
    let home = tempfile::tempdir()?;
    opencode_2_3_store(home.path())?;
    let lister = StoreSessionLister::new(StoreRoots {
        opencode: Some(home.path().join(".local/share/opencode")),
        ..StoreRoots::default()
    });
    let child = lister
        .describe(HarnessId::Opencode, CHILD, None)
        .await?
        .ok_or("child")?;
    assert_eq!(child.parent_session_id.as_deref(), Some(PARENT));
    let parent = lister
        .describe(HarnessId::Opencode, PARENT, None)
        .await?
        .ok_or("parent")?;
    assert_eq!(parent.parent_session_id, None);
    Ok(())
}

#[tokio::test]
async fn opencode_2_3_with_part_rows_stores_the_reply_from_session_part() -> TestResult {
    let batch = idle_batch_from(PART_ROWS_FIXTURE, Layout::AsRecorded, PART_ROWS_SESSION).await?;
    assert_eq!(
        rows(&batch),
        vec![
            row(
                "user",
                "-",
                "Run the shell command uname -sr, then reply with its output in one line."
            ),
            row(
                "assistant",
                "-",
                "[thinking] I need to run a shell command to get the system information."
            ),
            row("tool", "shell", "Linux 6.8.0-acme\n"),
            row(
                "assistant",
                "-",
                "[thinking] The command printed the kernel version, so I reply with it."
            ),
            row("assistant", "-", "Linux 6.8.0-acme"),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn opencode_2_3_part_rows_keep_the_same_rows_and_subagent_link() -> TestResult {
    for session in [PARENT, CHILD] {
        let inline = idle_batch_from(SUBAGENT_FIXTURE, Layout::AsRecorded, session).await?;
        let parts = idle_batch_from(SUBAGENT_FIXTURE, Layout::PartRows, session).await?;
        assert_eq!(rows(&parts), rows(&inline), "{session}");
        assert_eq!(
            at(&parts, "/settings/parent_session_id"),
            at(&inline, "/settings/parent_session_id")
        );
        assert_eq!(
            at(&parts, "/settings/tool_use_id"),
            at(&inline, "/settings/tool_use_id")
        );
    }
    let child = idle_batch_from(SUBAGENT_FIXTURE, Layout::PartRows, CHILD).await?;
    assert_eq!(
        at(&child, "/settings/tool_use_id"),
        "call_5d7c0668d51048c8aa5d86e4"
    );
    Ok(())
}
