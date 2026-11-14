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
use wire::harness::HarnessId;

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

type TestResult = Result<(), Box<dyn std::error::Error>>;

const NOW_TEXT: &str = "2026-11-14T18:00:00.000Z";
const LINK_KEYS: [&str; 5] = [
    "parent_session_id",
    "agent_id",
    "agent_type",
    "description",
    "tool_use_id",
];

const OPENCODE_PARENT: &str = "ses_e24ce149bffeqEEfoP0e2B2wD9";
const OPENCODE_CHILD: &str = "ses_e24cd5b95ffeiP2mFYLu9oTTPT";
const GROK_PARENT: &str = "01a1db35-3246-7211-8e3b-57f76df009c0";
const GROK_CHILD: &str = "01a1db35-4b0f-7930-94a7-973630474510";
const GROK_CWD: &str = "%2Fhome%2Fuser%2Facmekit";
const DSH_PARENT: &str = "session-00000000-0000-4000-8000-000000000001";
const DSH_CHILD: &str = "session-00000000-0000-4000-8000-000000000002";

const OPENCODE_1_SCHEMA: &str = "CREATE TABLE `session` (
  `id` text PRIMARY KEY, `project_id` text NOT NULL, `workspace_id` text, `parent_id` text,
  `slug` text NOT NULL, `directory` text NOT NULL, `path` text, `title` text NOT NULL,
  `version` text NOT NULL, `share_url` text, `summary_additions` integer, `summary_deletions` integer,
  `summary_files` integer, `summary_diffs` text, `metadata` text, `cost` real DEFAULT 0 NOT NULL,
  `tokens_input` integer DEFAULT 0 NOT NULL, `tokens_output` integer DEFAULT 0 NOT NULL,
  `tokens_reasoning` integer DEFAULT 0 NOT NULL, `tokens_cache_read` integer DEFAULT 0 NOT NULL,
  `tokens_cache_write` integer DEFAULT 0 NOT NULL, `revert` text, `permission` text, `agent` text,
  `model` text, `time_created` integer NOT NULL, `time_updated` integer NOT NULL,
  `time_compacting` integer, `time_archived` integer
);
CREATE TABLE `message` (
  `id` text PRIMARY KEY, `session_id` text NOT NULL, `time_created` integer NOT NULL,
  `time_updated` integer NOT NULL, `data` text NOT NULL
);
CREATE TABLE `part` (
  `id` text PRIMARY KEY, `message_id` text NOT NULL, `session_id` text NOT NULL,
  `time_created` integer NOT NULL, `time_updated` integer NOT NULL, `data` text NOT NULL
);
CREATE TABLE `session_message` (`id` text PRIMARY KEY, `session_id` text NOT NULL, `type` text NOT NULL, `seq` integer NOT NULL, `time_created` integer NOT NULL, `time_updated` integer NOT NULL, `data` text NOT NULL);";

#[derive(Debug, Deserialize)]
#[serde(tag = "table", rename_all = "snake_case")]
enum Opencode1Row {
    Session {
        id: String,
        parent_id: Option<String>,
        slug: String,
        directory: String,
        title: String,
        version: String,
        agent: Option<String>,
        time_created: i64,
        time_updated: i64,
    },
    Message {
        id: String,
        session_id: String,
        time_created: i64,
        time_updated: i64,
        data: String,
    },
    Part {
        id: String,
        message_id: String,
        session_id: String,
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

fn opencode_1_19_store(home: &Path) -> TestResult {
    let data = home.join(".local/share/opencode");
    std::fs::create_dir_all(&data)?;
    let connection = Connection::open(data.join("opencode.db"))?;
    connection.execute_batch(OPENCODE_1_SCHEMA)?;
    let rows = std::fs::read_to_string(testkit::recording(
        "capture/stores/opencode-v1-subagent.jsonl",
    ))?;
    for line in rows.lines() {
        match serde_json::from_str::<Opencode1Row>(line)? {
            Opencode1Row::Session {
                id,
                parent_id,
                slug,
                directory,
                title,
                version,
                agent,
                time_created,
                time_updated,
            } => connection.execute(
                "INSERT INTO session (id, project_id, parent_id, slug, directory, title, version, agent, time_created, time_updated) VALUES (?1, 'global', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                (
                    id,
                    parent_id,
                    slug,
                    directory,
                    title,
                    version,
                    agent,
                    time_created,
                    time_updated,
                ),
            )?,
            Opencode1Row::Message {
                id,
                session_id,
                time_created,
                time_updated,
                data,
            } => connection.execute(
                "INSERT INTO message VALUES (?1, ?2, ?3, ?4, ?5)",
                (id, session_id, time_created, time_updated, data),
            )?,
            Opencode1Row::Part {
                id,
                message_id,
                session_id,
                time_created,
                time_updated,
                data,
            } => connection.execute(
                "INSERT INTO part VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (id, message_id, session_id, time_created, time_updated, data),
            )?,
        };
    }
    Ok(())
}

fn grok_store(home: &Path) -> TestResult {
    let fixture = |name: &str| testkit::recording(&format!("capture/stores/grok-subagent/{name}"));
    let sessions = home.join(".grok/sessions").join(GROK_CWD);
    let parent = sessions.join(GROK_PARENT);
    let child = sessions.join(GROK_CHILD);
    let meta = parent.join("subagents").join(GROK_CHILD);
    for dir in [&parent, &child, &meta] {
        std::fs::create_dir_all(dir)?;
    }
    for (name, target) in [
        ("parent-updates.jsonl", parent.join("updates.jsonl")),
        ("parent-summary.json", parent.join("summary.json")),
        ("child-updates.jsonl", child.join("updates.jsonl")),
        ("child-summary.json", child.join("summary.json")),
        ("child-meta.json", meta.join("meta.json")),
    ] {
        std::fs::copy(fixture(name), target)?;
    }
    Ok(())
}

fn dsh_store(home: &Path, header_edit: impl Fn(&str) -> String) -> TestResult {
    let folder = home
        .join(".dsh/sessions/--home-user-acmekit--")
        .join(DSH_CHILD);
    std::fs::create_dir_all(&folder)?;
    let log = std::fs::read_to_string(testkit::recording(
        "capture/stores/dsh-subagent-child.jsonl",
    ))?;
    let log = header_edit(&log);
    std::fs::write(
        folder.join("session.v4.jsonl.zstd"),
        zstd::encode_all(log.as_bytes(), 0)?,
    )?;
    Ok(())
}

async fn batch(
    home: &Path,
    harness: StoreHarness,
    payload: &str,
) -> Result<Json, Box<dyn std::error::Error>> {
    let env = config::Environment::from_vars([("HOME".to_owned(), home.display().to_string())]);
    let transport = FakeTransport::default();
    let writer = CaptureWriter::new(
        transport.clone(),
        CaptureSpool::new(home.join("spool")),
        None,
        None,
    );
    let hook = StoreHook::new(
        harness,
        writer,
        &store_roots(&env),
        &config::Paths::new(&env)?,
    )
    .ok_or("store")?;
    let now: DateTime<Utc> = NOW_TEXT.parse()?;
    assert_eq!(hook.run(payload, now).await.stderr, "");
    let posts = transport.posts.lock().clone();
    let [body] = posts.as_slice() else {
        return Err(format!("expected one post, got {}", posts.len()).into());
    };
    Ok(serde_json::from_str(body)?)
}

fn link(batch: &Json) -> Vec<(&'static str, Option<String>)> {
    LINK_KEYS
        .iter()
        .map(|key| {
            (
                *key,
                batch
                    .pointer(&format!("/settings/{key}"))
                    .and_then(Json::as_str)
                    .map(str::to_owned),
            )
        })
        .collect()
}

fn expected(values: [Option<&str>; 5]) -> Vec<(&'static str, Option<String>)> {
    LINK_KEYS
        .iter()
        .zip(values)
        .map(|(key, value)| (*key, value.map(str::to_owned)))
        .collect()
}

fn opencode_idle(session: &str) -> String {
    format!(
        r#"{{"session_id":"{session}","hook_event_name":"session.idle","cwd":"/home/user/acmekit"}}"#
    )
}

fn grok_stop(session: &str) -> String {
    format!(r#"{{"hook_event_name":"Stop","session_id":"{session}","cwd":"/home/user/acmekit"}}"#)
}

#[tokio::test]
async fn opencode_1_19_subagent_settings_name_the_parent_and_the_task_call() -> TestResult {
    let home = tempfile::tempdir()?;
    opencode_1_19_store(home.path())?;
    let child = batch(
        home.path(),
        StoreHarness::Opencode,
        &opencode_idle(OPENCODE_CHILD),
    )
    .await?;
    assert_eq!(
        link(&child),
        expected([
            Some(OPENCODE_PARENT),
            Some(OPENCODE_CHILD),
            Some("general"),
            Some("Count entries in /etc"),
            Some("call_c8e9d65911c8484282fcd304"),
        ])
    );
    let parent = batch(
        home.path(),
        StoreHarness::Opencode,
        &opencode_idle(OPENCODE_PARENT),
    )
    .await?;
    assert_eq!(link(&parent), expected([None; 5]));
    Ok(())
}

#[tokio::test]
async fn opencode_1_19_listing_names_the_subagent_parent() -> TestResult {
    let home = tempfile::tempdir()?;
    opencode_1_19_store(home.path())?;
    let lister = StoreSessionLister::new(StoreRoots {
        opencode: Some(home.path().join(".local/share/opencode")),
        ..StoreRoots::default()
    });
    let child = lister
        .describe(HarnessId::Opencode, OPENCODE_CHILD, None)
        .await?
        .ok_or("child")?;
    assert_eq!(child.parent_session_id.as_deref(), Some(OPENCODE_PARENT));
    Ok(())
}

#[tokio::test]
async fn grok_subagent_settings_name_the_parent_from_its_spawn_record() -> TestResult {
    let home = tempfile::tempdir()?;
    grok_store(home.path())?;
    let child = batch(home.path(), StoreHarness::GrokBuild, &grok_stop(GROK_CHILD)).await?;
    assert_eq!(
        link(&child),
        expected([
            Some(GROK_PARENT),
            Some(GROK_CHILD),
            Some("general-purpose"),
            Some("Count entries in /etc"),
            None,
        ])
    );
    let parent = batch(
        home.path(),
        StoreHarness::GrokBuild,
        &grok_stop(GROK_PARENT),
    )
    .await?;
    assert_eq!(link(&parent), expected([None; 5]));
    Ok(())
}

#[tokio::test]
async fn dsh_subagent_settings_name_the_parent_from_the_session_header() -> TestResult {
    let home = tempfile::tempdir()?;
    dsh_store(home.path(), str::to_owned)?;
    let payload = format!(r#"{{"hook_event_name":"turn/end","session_id":"{DSH_CHILD}"}}"#);
    let child = batch(home.path(), StoreHarness::Dsh, &payload).await?;
    assert_eq!(
        link(&child),
        expected([Some(DSH_PARENT), Some(DSH_CHILD), None, None, None])
    );
    Ok(())
}

#[tokio::test]
async fn a_dsh_fork_whose_header_has_no_subagent_origin_is_not_linked() -> TestResult {
    let home = tempfile::tempdir()?;
    dsh_store(home.path(), |log| {
        log.replacen(r#""origin":"subagent","#, "", 1)
    })?;
    let payload = format!(r#"{{"hook_event_name":"turn/end","session_id":"{DSH_CHILD}"}}"#);
    let fork = batch(home.path(), StoreHarness::Dsh, &payload).await?;
    assert_eq!(link(&fork), expected([None; 5]));
    Ok(())
}
