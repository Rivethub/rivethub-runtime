use jiff::Timestamp;
use std::path::Path;

use capture::listing::StoreRoots;
use capture::listing::StoreSessionLister;
use capture::readers::SkipRule;
use capture::readers::StoreReader;
use kernel::transcript::ListLimit;
use kernel::transcript::SessionLister;
use kernel::transcript::ToolOutputRead;
use kernel::transcript::TranscriptRead;
use kernel::transcript::TranscriptReader;
use kernel::transcript::TranscriptToolStatus;
use kernel::transcript::TranscriptWindow;
use rusqlite::Connection;
use serde::Deserialize;
use wire::harness::HarnessId;
use wire::message::Role;
use wire::session_id::SessionId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SHELL_SESSION: &str = "ses_eab005e08ffeKovN6iWJ0LSV2M";
const FAILED_SESSION: &str = "ses_eab0003fcffeD6yRMW6MstID9X";

const SYSTEM_ROW: &str = r#"INSERT INTO session_message VALUES('msg_sys','ses_eab005e08ffeKovN6iWJ0LSV2M','system',24,1792427403500,1792427403500,'{"text":"The available tools have changed.","time":{"created":1792427403500}}')"#;
const UNKNOWN_ROW: &str = r#"INSERT INTO session_message VALUES('msg_new','ses_eab005e08ffeKovN6iWJ0LSV2M','compaction',25,1792427403550,1792427403550,'{"time":{"created":1792427403550}}')"#;
const STREAMING_ROW: &str = r#"INSERT INTO session_message VALUES('msg_live','ses_eab005e08ffeKovN6iWJ0LSV2M','assistant',26,1792427403600,1792427403600,'{"time":{"created":1792427403600,"streamed":1792427403610},"agent":"build","content":[{"type":"text","text":"Still"}],"cost":0}')"#;
const COMPLETED_DATA: &str = r#"UPDATE session_message SET data = '{"time":{"created":1792427403600,"streamed":1792427403610,"completed":1792427403700},"agent":"build","content":[{"type":"text","text":"Still here."}],"finish":"stop","cost":0}' WHERE id = 'msg_live'"#;

const AGENT_ROW: &str = r#"INSERT INTO session_message VALUES('msg_agent','ses_eab005e08ffeKovN6iWJ0LSV2M','agent-switched',24,1792436678173,1792436678173,'{"time":{"created":1792436678173},"agent":"plan"}')"#;
const MODEL_ROW: &str = r#"INSERT INTO session_message VALUES('msg_model','ses_eab005e08ffeKovN6iWJ0LSV2M','model-switched',37,1792436690265,1792436690265,'{"time":{"created":1792436690265},"model":{"id":"mock2","providerID":"mock"},"previous":{"id":"mock","providerID":"mock","variant":"default"}}')"#;
const ABORTED_ROW: &str = r#"INSERT INTO session_message VALUES('msg_cut','ses_eab005e08ffeKovN6iWJ0LSV2M','assistant',41,1792436690663,1792436690700,'{"time":{"created":1792436690663,"streamed":1792436690699},"agent":"plan","content":[{"type":"text","text":"Checking first."},{"type":"tool","id":"call_2","name":"shell","state":{"status":"streaming","input":"{\"comm"},"time":{"created":1792436690700}}]}')"#;
const INTERRUPTED_ROW: &str = r#"INSERT INTO session_message VALUES('msg_stop','ses_eab005e08ffeKovN6iWJ0LSV2M','idle',46,1792436690728,1792436690728,'{"time":{"created":1792436690728},"outcome":"interrupted"}')"#;

const SCHEMA: &str = "PRAGMA foreign_keys=OFF;
CREATE TABLE `session_v2` (
  `id` text PRIMARY KEY, `project_id` text NOT NULL, `workspace_id` text, `parent_id` text,
  `fork_session_id` text, `fork_boundary` text, `slug` text NOT NULL, `directory` text NOT NULL,
  `path` text, `title` text, `version` text NOT NULL, `share_url` text, `summary_additions` integer,
  `summary_deletions` integer, `summary_files` integer, `summary_diffs` text, `metadata` text,
  `cost` real DEFAULT 0 NOT NULL, `tokens_input` integer DEFAULT 0 NOT NULL,
  `tokens_output` integer DEFAULT 0 NOT NULL, `tokens_reasoning` integer DEFAULT 0 NOT NULL,
  `tokens_cache_read` integer DEFAULT 0 NOT NULL, `tokens_cache_write` integer DEFAULT 0 NOT NULL,
  `revert` text, `permission` text, `agent` text, `model` text, `time_created` integer NOT NULL,
  `time_updated` integer NOT NULL, `time_idle` integer, `time_viewed` integer, `idle_outcome` text,
  `time_compacting` integer, `time_archived` integer, `time_suspended` integer,
  `resume_attempts` integer DEFAULT 0 NOT NULL,
  CONSTRAINT `fk_session_v2_project_id_project_id_fk` FOREIGN KEY (`project_id`) REFERENCES `project`(`id`) ON DELETE CASCADE
);
CREATE TABLE `session_message` (
  `id` text PRIMARY KEY, `session_id` text NOT NULL, `type` text NOT NULL, `seq` integer NOT NULL,
  `time_created` integer NOT NULL, `time_updated` integer NOT NULL, `data` text NOT NULL,
  CONSTRAINT `fk_session_message_session_id_session_v2_id_fk` FOREIGN KEY (`session_id`) REFERENCES `session_v2`(`id`) ON DELETE CASCADE
);
CREATE UNIQUE INDEX `session_message_session_seq_idx` ON `session_message` (`session_id`,`seq`);";

const SESSION_INSERT: &str = "INSERT INTO session_v2 (id, project_id, slug, directory, title, version, model, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
const MESSAGE_INSERT: &str = "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

#[derive(Debug, Deserialize)]
#[serde(tag = "table")]
enum StoreRow {
    #[serde(rename = "session_v2")]
    Session {
        id: String,
        project_id: String,
        slug: String,
        directory: String,
        title: String,
        version: String,
        model: String,
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
}

fn store(root: &Path) -> Result<Connection, Box<dyn std::error::Error>> {
    let connection = Connection::open(root.join("opencode.db"))?;
    connection.execute_batch(SCHEMA)?;
    let rows = std::fs::read_to_string(testkit::recording("capture/stores/opencode-v2.jsonl"))?;
    for line in rows.lines() {
        match serde_json::from_str::<StoreRow>(line)? {
            StoreRow::Session {
                id,
                project_id,
                slug,
                directory,
                title,
                version,
                model,
                time_created,
                time_updated,
            } => connection.execute(
                SESSION_INSERT,
                (
                    id,
                    project_id,
                    slug,
                    directory,
                    title,
                    version,
                    model,
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
            } => connection.execute(
                MESSAGE_INSERT,
                (id, session_id, kind, seq, time_created, time_updated, data),
            )?,
        };
    }
    Ok(connection)
}

fn reader(root: &Path) -> Result<StoreReader, Box<dyn std::error::Error>> {
    StoreReader::open(HarnessId::Opencode, root.to_path_buf()).ok_or_else(|| "reader".into())
}

fn at(ms: i64) -> Result<Timestamp, Box<dyn std::error::Error>> {
    Timestamp::from_millisecond(ms).map_err(|_error| format!("{ms}").into())
}

fn session(native: &str) -> SessionId {
    SessionId::new(HarnessId::Opencode, native)
}

#[tokio::test]
async fn opencode_2_store_lists_sessions_from_session_v2() -> TestResult {
    let dir = tempfile::tempdir()?;
    let connection = store(dir.path())?;
    connection.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT, model TEXT, time_created, time_updated);\
         INSERT INTO session VALUES ('ses_legacy', 'old', NULL, 1, 2);",
    )?;
    let lister = StoreSessionLister::new(StoreRoots {
        opencode: Some(dir.path().to_path_buf()),
        ..StoreRoots::default()
    });
    let rows = lister
        .list(&[HarnessId::Opencode], ListLimit::DEFAULT)
        .await;
    let brief: Vec<_> = rows
        .iter()
        .map(|row| {
            (
                row.id.as_str(),
                row.title.as_str(),
                row.model.as_deref(),
                row.created_at,
                row.updated_at,
            )
        })
        .collect();
    assert_eq!(
        brief,
        vec![
            (
                FAILED_SESSION,
                "Mock title",
                Some("mock/mock"),
                Some(1_792_427_425_485),
                1_792_427_425_809
            ),
            (
                SHELL_SESSION,
                "Mock title",
                Some("mock/mock"),
                Some(1_792_427_402_462),
                1_792_427_402_842
            ),
        ]
    );
    let described = lister
        .describe(HarnessId::Opencode, SHELL_SESSION, None)
        .await?
        .ok_or("described")?;
    assert_eq!(described.updated_at, 1_792_427_402_842);
    Ok(())
}

#[tokio::test]
async fn opencode_2_store_reads_text_and_tool_turns() -> TestResult {
    let dir = tempfile::tempdir()?;
    store(dir.path())?;
    let reader = reader(dir.path())?;
    assert_eq!(
        reader.sessions(dir.path()).await?,
        vec![session(FAILED_SESSION), session(SHELL_SESSION)]
    );
    let turns: Vec<_> = reader
        .read(&session(SHELL_SESSION))
        .await?
        .into_iter()
        .map(|turn| (turn.role, turn.content, turn.at))
        .collect();
    assert_eq!(
        turns,
        vec![
            (
                Role::User,
                "\"Run echo for me please\"".to_owned(),
                at(1_792_427_402_734)?
            ),
            (
                Role::Assistant,
                "I will run a command.".to_owned(),
                at(1_792_427_402_882)?
            ),
            (
                Role::Tool,
                "hello-from-tool\n".to_owned(),
                at(1_792_427_402_882)?
            ),
            (
                Role::Assistant,
                "The command printed hello-from-tool.".to_owned(),
                at(1_792_427_403_249)?
            ),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn opencode_2_tool_calls_carry_name_input_and_result() -> TestResult {
    let dir = tempfile::tempdir()?;
    store(dir.path())?;
    let reader = reader(dir.path())?;
    let slice = reader.messages(&session(SHELL_SESSION), 0).await?;
    assert!(slice.skipped.is_empty(), "{:?}", slice.skipped);
    let tool = slice
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .ok_or("tool")?;
    assert_eq!(tool.tool_name.as_deref(), Some("shell"));
    assert_eq!(tool.tool_result.as_deref(), Some("hello-from-tool\n"));
    let arguments = tool.tool_args.as_ref().ok_or("arguments")?;
    assert_eq!(
        arguments.get("command").and_then(|value| value.as_str()),
        Some("echo hello-from-tool")
    );
    let again = reader
        .messages(&session(SHELL_SESSION), slice.next_offset)
        .await?;
    assert_eq!(again.messages.len(), 0);
    let failed = reader.messages(&session(FAILED_SESSION), 0).await?;
    let tool = failed
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .ok_or("failed tool")?;
    assert_eq!(tool.tool_name.as_deref(), Some("read"));
    assert!(
        tool.content
            .starts_with("Invalid arguments for tool \"read\""),
        "{}",
        tool.content
    );
    Ok(())
}

#[tokio::test]
async fn opencode_2_failed_call_is_stored_and_read_as_failed() -> TestResult {
    let dir = tempfile::tempdir()?;
    store(dir.path())?;
    let lister = StoreSessionLister::new(StoreRoots {
        opencode: Some(dir.path().to_path_buf()),
        ..StoreRoots::default()
    });
    let TranscriptRead::Read(read) = lister
        .transcript(
            HarnessId::Opencode,
            FAILED_SESSION,
            TranscriptWindow::Latest,
        )
        .await?
    else {
        return Err("opencode has a transcript source".into());
    };
    let call = read
        .turns
        .iter()
        .filter_map(|turn| turn.tools.as_ref())
        .flatten()
        .find(|tool| tool.name == "read")
        .ok_or("the failed call")?;
    assert_eq!(call.status, TranscriptToolStatus::Error);
    let id = call.id.as_deref().ok_or("the call has an id")?;
    let ToolOutputRead::Found(output) = lister
        .tool_output(HarnessId::Opencode, FAILED_SESSION, id)
        .await?
    else {
        return Err("the failed call has an output".into());
    };
    assert!(output.is_error);
    assert!(
        output
            .text
            .starts_with("Invalid arguments for tool \"read\""),
        "{}",
        output.text
    );
    Ok(())
}

#[tokio::test]
async fn opencode_2_streaming_message_waits_until_completed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let connection = store(dir.path())?;
    let reader = reader(dir.path())?;
    let shell = session(SHELL_SESSION);
    let first = reader.messages(&shell, 0).await?;
    connection.execute(SYSTEM_ROW, [])?;
    connection.execute(UNKNOWN_ROW, [])?;
    connection.execute(STREAMING_ROW, [])?;
    let live = reader.messages(&shell, first.next_offset).await?;
    let speakers: Vec<Role> = live.messages.iter().map(|message| message.role).collect();
    assert_eq!(speakers, vec![Role::System]);
    let rules: Vec<SkipRule> = live.skipped.iter().map(|record| record.rule).collect();
    assert_eq!(rules, vec![SkipRule::UnknownRecordType]);
    connection.execute(COMPLETED_DATA, [])?;
    let done = reader.messages(&shell, live.next_offset).await?;
    let texts: Vec<&str> = done
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect();
    assert_eq!(texts, vec!["Still here."]);
    assert_eq!(done.skipped.len(), 0);
    let caught_up = reader.messages(&shell, done.next_offset).await?;
    assert_eq!(caught_up.messages.len(), 0);
    Ok(())
}

#[tokio::test]
async fn opencode_2_switch_markers_and_aborted_replies_are_not_skipped() -> TestResult {
    let dir = tempfile::tempdir()?;
    let connection = store(dir.path())?;
    let reader = reader(dir.path())?;
    let shell = session(SHELL_SESSION);
    let first = reader.messages(&shell, 0).await?;
    for row in [AGENT_ROW, MODEL_ROW, ABORTED_ROW, INTERRUPTED_ROW] {
        connection.execute(row, [])?;
    }
    let later = reader.messages(&shell, first.next_offset).await?;
    assert!(later.skipped.is_empty(), "{:?}", later.skipped);
    let turns: Vec<(Role, &str)> = later
        .messages
        .iter()
        .map(|message| (message.role, message.content.as_str()))
        .collect();
    assert_eq!(turns, vec![(Role::Assistant, "Checking first.")]);
    let caught_up = reader.messages(&shell, later.next_offset).await?;
    assert_eq!(caught_up.messages.len(), 0);
    Ok(())
}
