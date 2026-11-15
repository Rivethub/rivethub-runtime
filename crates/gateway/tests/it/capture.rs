use crate::common;

use futures::executor::block_on;
use kernel::memory::MemoryStore;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::message::Role;

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

const MESSAGE_PINNED: &[&str] = &[
    "session_key empty",
    "session_key spaces",
    "agent empty",
    "agent spaces",
    "event_id empty",
];

const NULL_TITLE_READS_AS_ABSENT: &str = "title null";

#[derive(serde::Deserialize)]
struct RecordedCase {
    input: Box<serde_json::value::RawValue>,
    ok: bool,
    message: Option<String>,
}

#[derive(serde::Deserialize)]
struct Recording {
    cases: std::collections::BTreeMap<String, RecordedCase>,
}

fn parse_error(body: &[u8]) -> wire::gateway::GatewayError {
    serde_json::from_slice(body).unwrap_or_else(|_| {
        wire::gateway::GatewayError::new(String::from_utf8_lossy(body).into_owned())
    })
}

#[test]
fn capture_batch_cases_match_the_recording() {
    let path = testkit::recording("capture/capture-zod/capture-batch.json");
    let recording: Recording =
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
    for (name, case) in recording.cases {
        let response = post(case.input.get().as_bytes());
        if case.ok || name == NULL_TITLE_READS_AS_ABSENT {
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
        if MESSAGE_PINNED.contains(&name.as_str()) {
            let recorded = case.message.expect("exact case carries its message");
            if parsed.error != recorded {
                failures.push(format!(
                    "{name}: message differs\n    recorded: {recorded}\n    actual:   {}",
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
