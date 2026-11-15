use crate::common;

use std::path::PathBuf;

use futures::executor::block_on;
use kernel::memory::MemoryStore;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::json::JsonObject;
use wire::message::Role;

fn golden(name: &str) -> PathBuf {
    testkit::golden_path(env!("CARGO_MANIFEST_DIR"), "tests/golden").join(name)
}

fn masks() -> testkit::Masks {
    testkit::Masks {
        port: "5174".to_string(),
        hostname: Some("gold-host".to_string()),
        tmp: Some("/tmp/golden".to_string()),
    }
}

fn recorded(method: &str, target: &str, body: Vec<u8>) -> testkit::Request {
    testkit::Request {
        method: method.to_string(),
        target: target.to_string(),
        headers: vec![("Host".to_string(), "127.0.0.1:5174".to_string())],
        body,
        recipe: None,
        via: None,
    }
}

fn app() -> axum::Router {
    let (gateway, _) = common::gateway();
    gateway.router(false)
}

const EXACT_ZOD_TEXT: &[&str] = &[
    "session_key empty",
    "session_key spaces",
    "agent empty",
    "agent spaces",
    "event_id empty",
];

const DEVIATIONS: &[(&str, &str)] = &[
    (
        "title null",
        "deliberate deviation: wire's Option<String> maps an explicit null to absent, so the batch is accepted; the recorded zod rejects null with invalid_type (no harness writer sends title:null)",
    ),
    (
        "message created_at bad",
        "a non-instant is rejected by serde before a zod issue is built; exact zod issue JSON is the open B3 decision",
    ),
    (
        "message created_at no offset",
        "a timestamp without an offset is rejected by serde before a zod issue is built; exact zod issue JSON is the open B3 decision",
    ),
    (
        "created_at bad",
        "a non-instant is rejected by serde before a zod issue is built; exact zod issue JSON is the open B3 decision",
    ),
];

#[derive(serde::Deserialize)]
struct ZodCase {
    input: Box<serde_json::value::RawValue>,
    ok: bool,
    message: Option<String>,
}

#[derive(serde::Deserialize)]
struct ZodRecording {
    cases: std::collections::BTreeMap<String, ZodCase>,
}

fn parse_error(body: &[u8]) -> wire::gateway::GatewayError {
    serde_json::from_slice(body).unwrap_or_else(|_| {
        wire::gateway::GatewayError::new(String::from_utf8_lossy(body).into_owned())
    })
}

#[test]
fn capture_batch_cases_match_the_recording() {
    let path = testkit::recording("capture/capture-zod/capture-batch.json");
    let recording: ZodRecording =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("golden readable"))
            .expect("capture golden parses");
    let post = |body: &[u8]| {
        block_on(common::answer(
            app(),
            recorded("POST", "/api/capture", body.to_vec()),
        ))
        .expect("in-process answer")
    };
    let mut failures: Vec<String> = Vec::new();
    let mut deviated: Vec<&str> = Vec::new();
    for (name, case) in recording.cases {
        if let Some((_, reason)) = DEVIATIONS.iter().find(|(deviation, _)| *deviation == name) {
            deviated.push(reason);
            continue;
        }
        let response = post(case.input.get().as_bytes());
        if case.ok {
            if response.status != 200 {
                failures.push(format!("{name}: status {}", response.status));
                continue;
            }
            let parsed: wire::capture::CaptureResult =
                serde_json::from_slice(&response.body).expect("capture result body");
            if !parsed.ok || parsed.inserted != 1 || parsed.skipped != 0 {
                failures.push(format!(
                    "{name}: unexpected body {}",
                    String::from_utf8_lossy(&response.body)
                ));
            }
            continue;
        }
        if response.status != 400 {
            failures.push(format!("{name}: status {}", response.status));
            continue;
        }
        let parsed = parse_error(&response.body);
        let exact = EXACT_ZOD_TEXT.contains(&name.as_str());
        if exact {
            let recorded = case.message.expect("exact case carries its message");
            if parsed.error != recorded {
                failures.push(format!(
                    "{name}: zod text drifted\n    recorded: {recorded}\n    actual:   {}",
                    parsed.error
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "capture batch drift:\n{}",
        failures.join("\n")
    );
    assert_eq!(
        deviated.len(),
        DEVIATIONS.len(),
        "every listed deviation must be exercised: {deviated:?}"
    );
}

#[test]
fn capture_body_over_the_mebibyte_limit_is_413() {
    let oversized = batch(|batch| {
        batch.messages = vec![message(|message| {
            message.event_id = "e-big".into();
            message.content = "x".repeat(1024 * 1024);
        })];
    });
    let response = block_on(common::answer(
        app(),
        recorded(
            "POST",
            "/api/capture",
            serde_json::to_vec(&oversized).expect("body serializes"),
        ),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 413);
    let parsed = parse_error(&response.body);
    assert_eq!(parsed.error, "body too large");
}

#[test]
fn a_resent_batch_is_accepted_and_stores_each_event_once() -> Result<(), Box<dyn std::error::Error>>
{
    let body = serde_json::to_vec(&batch(|batch| {
        batch.messages = vec![
            message(|message| message.event_id = "resent-1".to_owned()),
            message(|message| message.event_id = "resent-2".to_owned()),
        ];
    }))?;
    let (gateway, store) = common::gateway();
    let mut counts = Vec::new();
    for _ in 0..2 {
        let response = block_on(common::answer(
            gateway.router(false),
            recorded("POST", "/api/capture", body.clone()),
        ))
        .expect("in-process answer");
        assert_eq!(response.status, 200);
        let parsed: wire::capture::CaptureResult = serde_json::from_slice(&response.body)?;
        counts.push((parsed.inserted, parsed.skipped));
    }
    assert_eq!(counts, vec![(2, 0), (0, 2)]);
    assert_eq!(block_on(store.browse(filter_all()))?.len(), 2);
    Ok(())
}

#[test]
fn capture_lone_surrogates_become_replacement_characters() -> Result<(), Box<dyn std::error::Error>>
{
    let body = concat!(
        "{\"session_key\":\"s-cap\",\"agent\":\"a\",\"messages\":[{\"event_id\":\"e3\",",
        "\"role\":\"user\",\"content\":\"x\\ud800y\\ud83d\\ude00z\"}]}"
    );
    let (gateway, store) = common::gateway();
    let response = block_on(common::answer(
        gateway.router(false),
        recorded("POST", "/api/capture", body.as_bytes().to_vec()),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: wire::capture::CaptureResult =
        serde_json::from_slice(&response.body).expect("capture result body");
    assert_eq!(parsed.inserted, 1);
    let entries = block_on(store.browse(filter_all()))?;
    let [entry] = entries.as_slice() else {
        return Err("expected one stored entry".into());
    };
    assert!(entry.content.contains('\u{FFFD}'));
    assert!(entry.content.contains('\u{1F600}'));
    Ok(())
}

#[test]
fn capture_escaped_backslashes_are_not_surrogate_escapes() -> Result<(), Box<dyn std::error::Error>>
{
    let (gateway, store) = common::gateway();
    let body = concat!(
        "{\"session_key\":\"s-bslash\",\"agent\":\"a\",\"messages\":[{\"event_id\":\"e4\",",
        "\"role\":\"user\",\"content\":\"a\\\\ud800 b\\\\ud83d\\\\ude00 c\"}]}"
    );
    let response = block_on(common::answer(
        gateway.router(false),
        recorded("POST", "/api/capture", body.as_bytes().to_vec()),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let entries = block_on(store.browse(filter_all()))?;
    let [entry] = entries.as_slice() else {
        return Err("expected one stored entry".into());
    };
    assert_eq!(entry.content, "a\\ud800 b\\ud83d\\ude00 c");
    Ok(())
}

#[test]
fn capture_real_surrogate_pairs_survive_the_sanitizer() -> Result<(), Box<dyn std::error::Error>> {
    let (gateway, store) = common::gateway();
    let body = concat!(
        "{\"session_key\":\"s-pair\",\"agent\":\"a\",\"messages\":[{\"event_id\":\"e5\",",
        "\"role\":\"user\",\"content\":\"\\ud83d\\ude00\"}]}"
    );
    let response = block_on(common::answer(
        gateway.router(false),
        recorded("POST", "/api/capture", body.as_bytes().to_vec()),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let entries = block_on(store.browse(filter_all()))?;
    let [entry] = entries.as_slice() else {
        return Err("expected one stored entry".into());
    };
    assert_eq!(entry.content, "\u{1F600}");
    Ok(())
}

fn filter_all() -> kernel::memory::BrowseFilter {
    kernel::memory::BrowseFilter {
        conversation_id: None,
        agent: None,
        role: None,
        tool_name: None,
        tag: None,
        window: None,
        since: None,
        before: None,
        include_tools: true,
        order: kernel::memory::MemoryBrowseOrder::Desc,
        limit: kernel::memory::Limit::clamped(Some(100), 100, 1..=100),
    }
}

fn batch(edit: impl FnOnce(&mut CaptureBatchRequest)) -> CaptureBatchRequest {
    let mut batch = CaptureBatchRequest {
        session_key: "s".to_owned(),
        agent: "a".to_owned(),
        channel: None,
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: Vec::new(),
    };
    edit(&mut batch);
    batch
}

fn message(edit: impl FnOnce(&mut CaptureMessage)) -> CaptureMessage {
    let mut message = CaptureMessage {
        event_id: "e".to_owned(),
        role: Role::User,
        content: "hi".to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    };
    edit(&mut message);
    message
}

#[expect(clippy::expect_used)]
fn object(raw: &str) -> JsonObject {
    serde_json::from_str(raw).expect("the test literal parses as an object")
}

#[test]
fn capture_writer_scenarios_produce_the_recorded_counts() {
    let path = golden("capture/writer-results.json");
    let results: std::collections::BTreeMap<String, testkit::Json> =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("golden readable"))
            .expect("writer results parse");
    let scenarios = writer_scenarios();
    assert_eq!(scenarios.len(), results.len());
    for (name, batch) in scenarios {
        let response = block_on(common::answer(
            app(),
            recorded(
                "POST",
                "/api/capture",
                serde_json::to_vec(&batch).expect("body serializes"),
            ),
        ))
        .expect("in-process answer");
        assert_eq!(response.status, 200, "{name}");
        let parsed: wire::capture::CaptureResult =
            serde_json::from_slice(&response.body).expect("capture result body");
        let recorded = results
            .get(&name)
            .cloned()
            .unwrap_or_else(testkit::Json::default);
        assert!(parsed.ok, "{name}");
        assert_eq!(
            parsed.inserted,
            recorded
                .get("inserted")
                .and_then(testkit::Json::as_u64)
                .expect("recorded inserted"),
            "{name}"
        );
        assert_eq!(
            parsed.skipped,
            recorded
                .get("skipped")
                .and_then(testkit::Json::as_u64)
                .expect("recorded skipped"),
            "{name}"
        );
        assert_eq!(
            parsed.conversation_id, batch.session_key,
            "{name}: the gateway echoes the session key as the conversation identity"
        );
    }
}

fn writer_scenarios() -> Vec<(String, CaptureBatchRequest)> {
    vec![
        single_scenario(),
        numbers_scenario(),
        surrogate_scenario(),
        chunk_scenario(),
        elide_scenario(),
        redaction_scenario(),
    ]
}

#[expect(clippy::expect_used)]
fn single_scenario() -> (String, CaptureBatchRequest) {
    (
        "single".to_owned(),
        batch(|batch| {
            batch.session_key = "claude-code:11111111-1111-1111-1111-111111111111".into();
            batch.agent = "rivet-claude".into();
            batch.channel = Some("claude-code".to_owned());
            batch.messages = vec![message(|message| {
                message.event_id = "e1".into();
                message.created_at = Some(
                    chrono::DateTime::parse_from_rfc3339("2026-10-10T12:00:00.000Z")
                        .expect("recorded instant")
                        .with_timezone(&chrono::Utc),
                );
            })];
        }),
    )
}

fn numbers_scenario() -> (String, CaptureBatchRequest) {
    (
        "numbers_and_keys".to_owned(),
        batch(|batch| {
            batch.session_key = "s-num".into();
            batch.messages = vec![message(|message| {
                message.event_id = "e2".into();
                message.role = Role::Assistant;
                message.content = String::new();
                message.tool_name = Some("Bash".to_owned());
                message.tool_args = Some(object(
                    r#"{"1":0.1,"2":2,"b":1,"a":12345678901234567000,"neg":0,"big":1e21,"small":1.5e-7,"nested":{"9":null,"10":true,"z":1}}"#,
                ));
                message.metadata = Some(object(r#"{"source":"golden"}"#));
            })];
        }),
    )
}

fn surrogate_scenario() -> (String, CaptureBatchRequest) {
    (
        "surrogate_cap".to_owned(),
        batch(|batch| {
            batch.session_key = "s-cap".into();
            batch.messages = vec![message(|message| {
                message.event_id = "e3".into();
                message.content = format!("x{}x", "x".repeat(15998));
                message.metadata =
                    Some(object(r#"{"truncated":true,"full_content_length":26000}"#));
            })];
        }),
    )
}

fn chunk_scenario() -> (String, CaptureBatchRequest) {
    (
        "finalize_multi_chunk".to_owned(),
        batch(|batch| {
            batch.session_key = "s-chunks".into();
            batch.finalize = Some(true);
            batch.title = Some("t".to_owned());
            batch.settings = Some(object(r#"{"cwd":"/tmp/x"}"#));
            batch.messages = (0..6)
                .map(|index| {
                    message(|message| {
                        message.event_id = format!("m{index}");
                        message.role = if index % 2 == 0 {
                            Role::User
                        } else {
                            Role::Assistant
                        };
                        message.content = format!("y{}y", "y".repeat(4096));
                    })
                })
                .collect();
        }),
    )
}

fn elide_scenario() -> (String, CaptureBatchRequest) {
    (
        "elide_tool_args".to_owned(),
        batch(|batch| {
            batch.session_key = "s-elide".into();
            batch.messages = vec![message(|message| {
                message.event_id = "e4".into();
                message.role = Role::Tool;
                message.content = "r".into();
                message.tool_name = Some("Read".to_owned());
                message.tool_args = Some(object(r#"{"blob":"zzzz"}"#));
            })];
        }),
    )
}

fn redaction_scenario() -> (String, CaptureBatchRequest) {
    (
        "redaction".to_owned(),
        batch(|batch| {
            batch.session_key = "s-red".into();
            batch.messages = vec![message(|message| {
                message.event_id = "e5".into();
                message.content =
                    "token Bearer abcdefghijklmnop1234 and AKIAEXAMPLE and code=SECRET42".into();
                message.tool_args = Some(object(
                    r#"{"api_key":"sk-demo","nested":{"password":"p","ok":"fine"}}"#,
                ));
            })];
        }),
    )
}

#[test]
fn capture_store_failures_map_to_status_codes() {
    let gateway = common::failing_gateway();
    let batch = batch(|batch| {
        batch.messages = vec![message(|message| {
            message.event_id = "e1".into();
        })];
    });
    let response = block_on(common::answer(
        gateway.router(false),
        recorded(
            "POST",
            "/api/capture",
            serde_json::to_vec(&batch).expect("body serializes"),
        ),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 503);
    let parsed = parse_error(&response.body);
    assert_eq!(parsed.error, "memory store unavailable");
}

#[test]
fn masks_cover_the_healthz_body() {
    let masks = masks();
    let recorded =
        "{\"ok\":true,\"sessions\":0,\"name\":\"{{HOSTNAME}}\",\"node\":\"golden-node\"}";
    assert!(
        testkit::masked_eq(
            recorded,
            "{\"ok\":true,\"sessions\":0,\"name\":\"gold-host\",\"node\":\"golden-node\"}",
            &masks
        )
        .expect("recorded mask")
    );
}
