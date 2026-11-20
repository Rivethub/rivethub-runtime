use jiff::Timestamp;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use testkit::Json;
use wire::capture::CaptureResult;

use capture::hook::store::StoreHarness;
use capture::hook::store::StoreHook;
use capture::listing::store_roots;
use capture::spool::CaptureSpool;
use capture::writer::CaptureTransport;
use capture::writer::CaptureWriter;
use capture::writer::PostError;

const SESSION: &str = "33333333-3333-3333-3333-333333333333";
const NOW_TEXT: &str = "2026-10-11T12:05:00.000Z";

type TestResult = Result<(), Box<dyn std::error::Error>>;

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

fn hook(
    home: &Path,
    transport: &FakeTransport,
) -> Result<StoreHook<FakeTransport>, Box<dyn std::error::Error>> {
    let session = home.join(".grok/sessions/%2Fwork%2Fdemo").join(SESSION);
    std::fs::create_dir_all(&session)?;
    std::fs::copy(
        testkit::recording("capture/stores/grok-updates-open.jsonl"),
        session.join("updates.jsonl"),
    )?;
    std::fs::write(
        session.join("summary.json"),
        format!(
            r#"{{"info":{{"id":"{SESSION}","cwd":"/work/demo"}},"generated_title":"List the demo files","updated_at":"2026-10-11T12:00:30Z"}}"#
        ),
    )?;
    let env = config::Environment::from_vars([("HOME".to_owned(), home.display().to_string())]);
    let writer = CaptureWriter::new(
        transport.clone(),
        CaptureSpool::new(home.join("spool")),
        None,
        None,
    );
    StoreHook::new(
        StoreHarness::GrokBuild,
        writer,
        &store_roots(&env),
        &config::Paths::new(&env)?,
    )
    .ok_or_else(|| "grok home".into())
}

fn at<'a>(batch: &'a Json, pointer: &str) -> &'a Json {
    batch.pointer(pointer).unwrap_or(&Json::Null)
}

fn payload(event: &str) -> String {
    format!(
        r#"{{"hookEventName":"stop","hook_event_name":"{event}","sessionId":"{SESSION}","session_id":"{SESSION}","cwd":"/work/demo"}}"#
    )
}

#[tokio::test]
async fn a_turn_end_posts_the_whole_log_as_a_titled_grok_conversation() -> TestResult {
    let home = tempfile::tempdir()?;
    let transport = FakeTransport::default();
    let hook = hook(home.path(), &transport)?;
    let now: Timestamp = NOW_TEXT.parse()?;
    let run = hook.run(&payload("Stop"), now).await;
    assert_eq!(run.stderr, "");
    let posts = transport.posts.lock().clone();
    let [body] = posts.as_slice() else {
        return Err(format!("expected one post, got {}", posts.len()).into());
    };
    let batch: Json = serde_json::from_str(body)?;
    assert_eq!(
        at(&batch, "/session_key").as_str(),
        Some(format!("grok-build:{SESSION}").as_str())
    );
    assert_eq!(at(&batch, "/agent"), "rivet-grok");
    assert_eq!(at(&batch, "/channel"), "grok-build");
    assert_eq!(at(&batch, "/title"), "List the demo files");
    assert_eq!(at(&batch, "/settings/cwd"), "/work/demo");
    assert_eq!(at(&batch, "/settings/last_event"), "Stop");
    assert!(at(&batch, "/finalize").is_null());
    let ids: Vec<&str> = at(&batch, "/messages")
        .as_array()
        .ok_or("messages")?
        .iter()
        .filter_map(|message| at(message, "/event_id").as_str())
        .collect();
    assert_eq!(ids.len(), 15);
    assert_eq!(
        ids.first(),
        Some(&format!("grok-build:{SESSION}:0").as_str())
    );
    assert_eq!(
        ids.last(),
        Some(&format!("grok-build:{SESSION}:1010029").as_str())
    );
    Ok(())
}

#[tokio::test]
async fn only_turn_ends_compaction_and_session_end_capture() -> TestResult {
    let home = tempfile::tempdir()?;
    let transport = FakeTransport::default();
    let hook = hook(home.path(), &transport)?;
    let now: Timestamp = NOW_TEXT.parse()?;
    for event in ["SessionStart", "UserPromptSubmit", "PostToolUse"] {
        assert_eq!(hook.run(&payload(event), now).await.stderr, "");
    }
    assert!(transport.posts.lock().is_empty());
    assert_eq!(hook.run(&payload("PreCompact"), now).await.stderr, "");
    assert_eq!(hook.run(&payload("SessionEnd"), now).await.stderr, "");
    let posts = transport.posts.lock().clone();
    let finals: Vec<Json> = posts
        .iter()
        .map(|body| serde_json::from_str::<Json>(body).map(|batch| at(&batch, "/finalize").clone()))
        .collect::<Result<_, _>>()?;
    assert_eq!(finals, [Json::Null, Json::Bool(true)]);
    Ok(())
}

#[tokio::test]
async fn top_down_settle_a_session_end_with_no_turns_posts_an_empty_marker() -> TestResult {
    let home = tempfile::tempdir()?;
    let transport = FakeTransport::default();
    let hook = hook(home.path(), &transport)?;
    let session = home
        .path()
        .join(".grok/sessions/%2Fwork%2Fdemo")
        .join(SESSION);
    std::fs::write(
        session.join("updates.jsonl"),
        "{\"method\":\"_x.ai/session/update\",\"params\":{\"sessionId\":\"s\",\"update\":{\"sessionUpdate\":\"hook_execution\",\"event_name\":\"session_start\",\"runs\":[]}}}\n",
    )?;
    let now: Timestamp = NOW_TEXT.parse()?;
    assert_eq!(hook.run(&payload("Stop"), now).await.stderr, "");
    assert!(transport.posts.lock().is_empty());
    assert_eq!(hook.run(&payload("SessionEnd"), now).await.stderr, "");
    let posts = transport.posts.lock();
    let [body] = posts.as_slice() else {
        return Err("expected one end".into());
    };
    let batch: wire::capture::CaptureBatchRequest = serde_json::from_str(body)?;
    drop(posts);
    assert_eq!(batch.messages, []);
    assert!(
        batch
            .session_observation()?
            .and_then(|o| o.end_key)
            .is_some()
    );
    Ok(())
}

const OPENCODE_SESSION: &str = "ses_e72147544ffepvBwqn0BclXFfX";

fn opencode_store(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let data = home.join(".local/share/opencode");
    std::fs::create_dir_all(&data)?;
    let connection = rusqlite::Connection::open(data.join("opencode.db"))?;
    connection.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT, model TEXT, time_created INTEGER, time_updated INTEGER);\
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);\
         CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);",
    )?;
    connection.execute(
        "INSERT INTO session VALUES (?1, 'Pong reply and session summary', NULL, 1791979200000, 1791979203000)",
        [OPENCODE_SESSION],
    )?;
    let message = "INSERT INTO message VALUES (?1, ?2, ?3, ?3, ?4)";
    connection.execute(
        message,
        (
            "msg_1",
            OPENCODE_SESSION,
            1_791_979_200_000_i64,
            r#"{"role":"user"}"#,
        ),
    )?;
    connection.execute(
        message,
        (
            "msg_2",
            OPENCODE_SESSION,
            1_791_979_201_000_i64,
            r#"{"role":"assistant"}"#,
        ),
    )?;
    let part = "INSERT INTO part VALUES (?1, ?2, ?3, ?4, ?4, ?5)";
    for (id, message, at, data) in [
        (
            "prt_1",
            "msg_1",
            1_791_979_200_000_i64,
            r#"{"type":"text","text":"Reply with pong."}"#,
        ),
        (
            "prt_2",
            "msg_2",
            1_791_979_201_000,
            r#"{"type":"step-start"}"#,
        ),
        (
            "prt_2b",
            "msg_2",
            1_791_979_201_050,
            r#"{"type":"text","text":"\n\n"}"#,
        ),
        (
            "prt_3",
            "msg_2",
            1_791_979_201_100,
            r#"{"type":"reasoning","text":" The user wants pong. "}"#,
        ),
        (
            "prt_4",
            "msg_2",
            1_791_979_201_200,
            r#"{"type":"tool","tool":"bash","callID":"call-pong","state":{"status":"completed","input":{"command":"echo pong"},"output":"pong"}}"#,
        ),
        (
            "prt_5",
            "msg_2",
            1_791_979_201_300,
            r#"{"type":"tool","tool":"read","state":{"status":"error","input":{"path":"/x"},"error":"no such file"}}"#,
        ),
        (
            "prt_6",
            "msg_2",
            1_791_979_201_400,
            r#"{"type":"text","text":"pong"}"#,
        ),
    ] {
        connection.execute(part, (id, message, OPENCODE_SESSION, at, data))?;
    }
    Ok(())
}

#[tokio::test]
async fn an_idle_opencode_session_posts_its_parts_under_their_part_ids() -> TestResult {
    let home = tempfile::tempdir()?;
    opencode_store(home.path())?;
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
    let now: Timestamp = NOW_TEXT.parse()?;
    let payload = |event: &str| {
        format!(
            r#"{{"session_id":"{OPENCODE_SESSION}","hook_event_name":"{event}","cwd":"/work/local"}}"#
        )
    };
    assert_eq!(hook.run(&payload("session.updated"), now).await.stderr, "");
    assert!(transport.posts.lock().is_empty());
    assert_eq!(hook.run(&payload("session.idle"), now).await.stderr, "");
    let posts = transport.posts.lock().clone();
    let [body] = posts.as_slice() else {
        return Err(format!("expected one post, got {}", posts.len()).into());
    };
    let batch: Json = serde_json::from_str(body)?;
    assert_eq!(
        at(&batch, "/session_key").as_str(),
        Some(format!("opencode:{OPENCODE_SESSION}").as_str())
    );
    assert_eq!(at(&batch, "/agent"), "rivet-glm");
    assert_eq!(at(&batch, "/channel"), "opencode");
    assert_eq!(at(&batch, "/title"), "Pong reply and session summary");
    assert_eq!(at(&batch, "/settings/cwd"), "/work/local");
    assert_eq!(at(&batch, "/messages/2/metadata/tool_use_id"), "call-pong");
    assert_eq!(at(&batch, "/messages/2/metadata/session_store_position"), 5);
    assert_eq!(
        at(&batch, "/messages/2/metadata/store_format"),
        "opencode-part"
    );
    let rows: Vec<(&str, &str, &str)> = at(&batch, "/messages")
        .as_array()
        .ok_or("messages")?
        .iter()
        .filter_map(|message| {
            Some((
                at(message, "/event_id").as_str()?,
                at(message, "/role").as_str()?,
                at(message, "/content").as_str()?,
            ))
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("prt_1", "user", "Reply with pong."),
            ("prt_3", "assistant", "[thinking] The user wants pong."),
            ("prt_4", "tool", "[tool-result] bash"),
            ("prt_5", "tool", "[tool-failure] read"),
            ("prt_6", "assistant", "pong"),
        ]
    );
    Ok(())
}

const CODEX_SESSION: &str = "01a18e31-2073-7cc2-be8a-8e36c42f8948";

const ROLLOUT: &str = r#"{"timestamp":"2026-10-30T15:00:00.000Z","type":"session_meta","payload":{"id":"01a18e31-2073-7cc2-be8a-8e36c42f8948","cwd":"/work/codex"}}
{"timestamp":"2026-10-30T15:00:01.000Z","type":"response_item","payload":{"type":"message","role":"developer","id":"msg_dev","content":[{"type":"input_text","text":"rules"}]}}
{"timestamp":"2026-10-30T15:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","id":"msg_env","content":[{"type":"input_text","text":"<environment_context>cwd</environment_context>"}]}}
{"timestamp":"2026-10-30T15:00:03.000Z","type":"response_item","payload":{"type":"message","role":"user","id":"msg_user","content":[{"type":"input_text","text":"  apply the patch  "}]}}
{"timestamp":"2026-10-30T15:00:04.000Z","type":"response_item","payload":{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"Patch "},{"type":"summary_text","text":"first."}],"content":null}}
{"timestamp":"2026-10-30T15:00:05.000Z","type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","call_id":"call_1","name":"apply_patch","input":"*** Begin Patch"}}
{"timestamp":"2026-10-30T15:00:06.000Z","type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":"Done!"}}
{"timestamp":"2026-10-30T15:00:07.000Z","type":"response_item","payload":{"type":"function_call","call_id":"call_2","name":"shell","arguments":"{\"command\":[\"ls\"]}"}}
{"timestamp":"2026-10-30T15:00:08.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_9","output":{"error":"no"},"error":"denied"}}
{"timestamp":"2026-10-30T15:00:09.000Z","type":"event_msg","payload":{"type":"agent_message","message":"Applied."}}
{"timestamp":"2026-10-30T15:00:10.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Applied."}]}}
"#;

#[tokio::test]
async fn a_codex_turn_end_posts_its_rollout_items() -> TestResult {
    let home = tempfile::tempdir()?;
    let day = home.path().join(".codex/sessions/2026/10/30");
    std::fs::create_dir_all(&day)?;
    std::fs::write(
        day.join(format!("rollout-2026-10-30T15-00-00-{CODEX_SESSION}.jsonl")),
        ROLLOUT,
    )?;
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
        StoreHarness::Codex,
        writer,
        &store_roots(&env),
        &config::Paths::new(&env)?,
    )
    .ok_or("codex store")?;
    let now: Timestamp = NOW_TEXT.parse()?;
    let payload = |event: &str| {
        format!(
            r#"{{"session_id":"{CODEX_SESSION}","hook_event_name":"{event}","cwd":"/work/codex"}}"#
        )
    };
    for event in ["UserPromptSubmit", "Stop", "SessionEnd"] {
        assert_eq!(hook.run(&payload(event), now).await.stderr, "");
    }
    let posts = transport.posts.lock().clone();
    let [stop, end] = posts.as_slice() else {
        return Err(format!("expected two posts, got {}", posts.len()).into());
    };
    let batch: Json = serde_json::from_str(stop)?;
    assert_eq!(
        at(&batch, "/session_key").as_str(),
        Some(format!("codex:{CODEX_SESSION}").as_str())
    );
    assert_eq!(at(&batch, "/agent"), "rivet-gpt");
    assert_eq!(at(&batch, "/channel"), "codex");
    assert_eq!(at(&batch, "/title"), "apply the patch");
    assert!(at(&batch, "/finalize").is_null());
    let rows: Vec<(&str, &str, &str)> = at(&batch, "/messages")
        .as_array()
        .ok_or("messages")?
        .iter()
        .filter_map(|message| {
            Some((
                at(message, "/event_id").as_str()?,
                at(message, "/role").as_str()?,
                at(message, "/content").as_str()?,
            ))
        })
        .collect();
    let line = format!("codex:{CODEX_SESSION}:line:10");
    assert_eq!(
        rows,
        [
            ("msg_user", "user", "apply the patch"),
            ("rs_1", "assistant", "[thinking] Patch first."),
            ("ctc_1", "tool", "[tool] apply_patch"),
            ("ctco_1", "tool", "[tool-result] apply_patch"),
            ("call_2", "tool", "[tool] shell"),
            ("call_9", "tool", "[tool-failure] unknown"),
            (line.as_str(), "assistant", "Applied."),
        ]
    );
    let messages = at(&batch, "/messages");
    assert_eq!(at(messages, "/2/tool_args/input"), "*** Begin Patch");
    assert_eq!(at(messages, "/3/tool_result"), "Done!");
    assert_eq!(at(messages, "/4/tool_args/command/0"), "ls");
    assert_eq!(at(messages, "/5/tool_result"), r#"{"error":"no"}"#);
    assert_eq!(at(messages, "/0/metadata/session_jsonl_line"), 3);
    assert_eq!(at(messages, "/2/metadata/tool_use_id"), "call_1");
    let end: Json = serde_json::from_str(end)?;
    assert_eq!(at(&end, "/finalize"), true);
    Ok(())
}

const DSH_SESSION: &str = "session-09517129-cb94-487b-a8da-10a015a7d984";

const DSH_LOG: &str = r#"{"type":"session","version":4,"id":"session-09517129-cb94-487b-a8da-10a015a7d984","cwd":"/work/dsh"}
{"type":"user/message","seq":1,"time":1793018984235,"data":{"content":[{"type":"text","text":" list the files "}],"source":{"kind":"user"},"role":"user","id":"u-1"}}
{"type":"user/message","seq":2,"data":{"content":"a plugin snapshot","source":{"kind":"plugin"},"id":"u-2"}}
{"type":"session/title","seq":3,"data":{"title":"List the files"}}
{"type":"assistant/message","seq":4,"data":{"turn":1,"message":{"role":"assistant","content":[{"type":"reasoning","text":"Look first."},{"type":"text","text":"Listing."},{"type":"tool-call","id":"call_1","name":"ls"}],"id":"a-1"}}}
{"type":"tool/call","seq":5,"data":{"callId":"call_1","name":"ls","arguments":"{\"path\":\".\"}"}}
{"type":"tool/result","seq":6,"data":{"message":{"role":"tool","source":{"kind":"tool","callId":"call_1"},"content":[{"type":"text","text":"a.txt"}],"id":"r-1"}}}
{"type":"tool/result","seq":7,"data":{"message":{"role":"tool","source":{"kind":"tool","callId":"call_9"},"content":[{"type":"text","text":"boom","isError":true}],"id":"r-9"}}}
{"type":"assistant/message","seq":8,"data":{"message":{"role":"assistant","content":[{"type":"tool-call","id":"call_2","name":"ls"}],"id":"a-2"}}}
{"type":"turn/end","seq":9,"data":{"turn":1,"reason":{"kind":"completed"}}}
"#;

fn dsh_hook(
    home: &Path,
    transport: &FakeTransport,
) -> Result<StoreHook<FakeTransport>, Box<dyn std::error::Error>> {
    let folder = home.join(".dsh/sessions/--work-dsh--").join(DSH_SESSION);
    std::fs::create_dir_all(&folder)?;
    let mut log = zstd::encode_all(DSH_LOG.as_bytes(), 0)?;
    let writing = zstd::encode_all(&br#"{"type":"turn/end","seq":10,"data":{}}"#[..], 0)?;
    log.extend(writing.iter().take(writing.len() / 2));
    std::fs::write(folder.join("session.v4.jsonl.zstd"), log)?;
    let env = config::Environment::from_vars([("HOME".to_owned(), home.display().to_string())]);
    let writer = CaptureWriter::new(
        transport.clone(),
        CaptureSpool::new(home.join("spool")),
        None,
        None,
    );
    StoreHook::new(
        StoreHarness::Dsh,
        writer,
        &store_roots(&env),
        &config::Paths::new(&env)?,
    )
    .ok_or_else(|| "dsh store".into())
}

#[tokio::test]
async fn a_dsh_turn_end_posts_its_transcript() -> TestResult {
    let home = tempfile::tempdir()?;
    let transport = FakeTransport::default();
    let hook = dsh_hook(home.path(), &transport)?;
    let now: Timestamp = NOW_TEXT.parse()?;
    for event in ["user/message", "turn/end"] {
        let payload = format!(
            r#"{{"session_id":"{DSH_SESSION}","hook_event_name":"{event}","cwd":"/work/dsh"}}"#
        );
        assert_eq!(hook.run(&payload, now).await.stderr, "");
    }
    let posts = transport.posts.lock().clone();
    let [body] = posts.as_slice() else {
        return Err(format!("expected one post, got {}", posts.len()).into());
    };
    let batch: Json = serde_json::from_str(body)?;
    assert_eq!(
        at(&batch, "/session_key").as_str(),
        Some(format!("dsh:{DSH_SESSION}").as_str())
    );
    assert_eq!(at(&batch, "/agent"), "rivet-deepseek");
    assert_eq!(at(&batch, "/channel"), "dsh");
    assert_eq!(at(&batch, "/title"), "List the files");
    assert_eq!(at(&batch, "/finalize"), true);
    let rows: Vec<(&str, &str, &str)> = at(&batch, "/messages")
        .as_array()
        .ok_or("messages")?
        .iter()
        .filter_map(|message| {
            Some((
                at(message, "/event_id").as_str()?,
                at(message, "/role").as_str()?,
                at(message, "/content").as_str()?,
            ))
        })
        .collect();
    let id = |suffix: &str| format!("dsh:{DSH_SESSION}:{suffix}");
    let ids = ["u-1", "seq:3", "a-1", "call_1", "r-9", "seq:9"].map(id);
    assert_eq!(
        rows,
        [
            (ids[0].as_str(), "user", "list the files"),
            (
                ids[1].as_str(),
                "system",
                "[dsh.session/title] List the files"
            ),
            (
                ids[2].as_str(),
                "assistant",
                "[thinking] Look first.\nListing."
            ),
            (ids[3].as_str(), "tool", "[tool] ls"),
            (ids[4].as_str(), "tool", "[tool-failure] unknown"),
            (ids[5].as_str(), "system", "[dsh.turn/end] completed"),
        ]
    );
    let messages = at(&batch, "/messages");
    assert_eq!(at(messages, "/3/tool_args/path"), ".");
    assert_eq!(at(messages, "/3/tool_result"), "a.txt");
    assert_eq!(at(messages, "/4/tool_result"), "boom");
    assert_eq!(at(messages, "/0/metadata/session_jsonl_line"), 1);
    assert_eq!(at(messages, "/3/metadata/tool_use_id"), "call_1");
    Ok(())
}
