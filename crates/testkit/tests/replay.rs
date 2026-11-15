#[path = "replay/drift.rs"]
mod drift;

use std::path::Path;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use libtest_mimic::Arguments;
use libtest_mimic::Completion;
use libtest_mimic::Failed;
use libtest_mimic::Trial;
use libtest_mimic::run;
use serde_json::json;

use testkit::Frame;
use testkit::Handshake;
use testkit::HookFile;
use testkit::HookOutcome;
use testkit::HookPost;
use testkit::Json;
use testkit::Mismatch;
use testkit::PgOutcome;
use testkit::PgStep;
use testkit::ProviderTurn;
use testkit::Response;
use testkit::WsCall;
use testkit::WsReply;
use testkit::ws::FrameRecord;

fn fixture(name: &str) -> PathBuf {
    testkit::golden_path(env!("CARGO_MANIFEST_DIR"), "tests/fixtures").join(name)
}

fn masks() -> testkit::Masks {
    testkit::Masks {
        port: "5174".to_string(),
        hostname: Some("gold-host".to_string()),
        tmp: Some("/tmp/golden".to_string()),
    }
}

fn must_load<T>(loaded: Result<T, testkit::Error>, what: &str) -> Result<T, Failed> {
    loaded.map_err(|error| Failed::from(format!("{what}: {error}")))
}

fn must_find<'a, T>(found: Option<&'a T>, what: &str) -> Result<&'a T, Failed> {
    found.ok_or_else(|| Failed::from(format!("{what}: not found in the fixture")))
}

fn must_pass(outcome: Result<(), Mismatch>) -> Result<(), Failed> {
    match outcome {
        Ok(()) => Ok(()),
        Err(mismatch) => Err(Failed::from(format!(
            "{}: {}",
            mismatch.id, mismatch.detail
        ))),
    }
}

fn must_fail(outcome: Result<(), Mismatch>, why: &str) -> Result<Mismatch, Failed> {
    match outcome {
        Ok(()) => Err(Failed::from(format!(
            "{why}: expected a mismatch, got a match"
        ))),
        Err(mismatch) => Ok(mismatch),
    }
}

fn assert_detail(mismatch: &Mismatch, needle: &str, what: &str) -> Result<(), Failed> {
    if mismatch.detail.contains(needle) {
        Ok(())
    } else {
        Err(Failed::from(format!(
            "{what}: expected {needle:?} in the detail, got: {}",
            mismatch.detail
        )))
    }
}

fn load_failed(trial: String, error: testkit::Error) -> Trial {
    Trial::test(trial, move || Err(Failed::from(error.to_string())))
}

fn http_healthz() -> Response {
    Response {
        status: 200,
        headers: vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "Date".to_string(),
                "Mon, 12 Oct 2026 00:00:00 GMT".to_string(),
            ),
        ],
        body: br#"{"ok":true,"sessions":0,"name":"gold-host"}"#.to_vec(),
    }
}

#[expect(clippy::expect_used)]
fn http_png() -> Response {
    let body = BASE64
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==")
        .expect("the test png constant decodes");
    Response {
        status: 200,
        headers: vec![("Content-Type".to_string(), "image/png".to_string())],
        body,
    }
}

fn http_handler(request: &testkit::Request) -> Response {
    if request.target == "/healthz" {
        similar_asserts::assert_eq!(request.method, "GET");
        http_healthz()
    } else {
        similar_asserts::assert_eq!(request.target, "/pixel.png");
        http_png()
    }
}

fn add_http(trials: &mut Vec<Trial>, path: &Path) -> Result<(), Failed> {
    match testkit::http::cases(path) {
        Ok(cases) => {
            let skipped: SkippedCase =
                serde_json::from_str(include_str!("fixtures/http-skipped.json"))
                    .map_err(|error| Failed::from(error.to_string()))?;
            assert_ne!(skipped.reason.trim(), "");
            assert_eq!(
                cases.iter().filter(|case| case.id == skipped.name).count(),
                1
            );
            for case in cases {
                let trial = format!("http {}", case.id);
                let masks = masks();
                let deviation = case.id == skipped.name;
                let reason = skipped.reason.clone();
                trials.push(Trial::ignorable_test(trial, move || {
                    if deviation {
                        return Ok(Completion::ignored_with(reason));
                    }
                    must_pass(testkit::http::replay(&case, &masks, http_handler))?;
                    Ok(Completion::Completed)
                }));
            }
        }
        Err(error) => trials.push(load_failed(format!("http load {}", path.display()), error)),
    }
    Ok(())
}

fn build_recipe_body() -> Result<Vec<u8>, Failed> {
    #[derive(serde::Serialize)]
    struct ActivityEvent {
        v: u8,
        session: &'static str,
        #[serde(rename = "type")]
        kind: &'static str,
        activity: &'static str,
        ts: i64,
    }
    let events = ["idle", "thinking"]
        .into_iter()
        .zip(0_i64..)
        .map(|(activity, index)| ActivityEvent {
            v: 1,
            session: "golden-batch",
            kind: "activity",
            activity,
            ts: 1_760_000_100_000 + index,
        })
        .collect::<Vec<_>>();
    serde_json::to_vec(&events).map_err(|error| Failed::from(error.to_string()))
}

fn add_recipe(trials: &mut Vec<Trial>, path: &Path) {
    match testkit::http::cases(path) {
        Ok(cases) => {
            for case in cases {
                let trial = format!("http recipe {}", case.id);
                let masks = masks();
                trials.push(Trial::test(trial, move || {
                    let body = build_recipe_body()?;
                    let parsed: Vec<Json> = serde_json::from_slice(&body)
                        .map_err(|error| Failed::from(error.to_string()))?;
                    assert_eq!(parsed.len(), 2);
                    must_pass(testkit::http::replay(&case, &masks, |request| {
                        assert!(request.recipe.is_some());
                        assert_eq!(request.body, Vec::<u8>::new());
                        Response {
                            status: 400,
                            headers: vec![(
                                "Content-Type".to_string(),
                                "application/json".to_string(),
                            )],
                            body: br#"{"error":"expected an array of 1-100 events"}"#.to_vec(),
                        }
                    }))
                }));
            }
        }
        Err(error) => trials.push(load_failed(
            format!("recipe load {}", path.display()),
            error,
        )),
    }
}

fn recorded_frames(frames: &[FrameRecord]) -> Result<Vec<Frame>, Failed> {
    frames
        .iter()
        .map(|recorded| {
            Ok(match recorded {
                FrameRecord::Text { text } => Frame::Text(text.clone()),
                FrameRecord::Binary {
                    text,
                    base64,
                    replay,
                } => {
                    let data = match (text, base64) {
                        (Some(text), None) => text.as_bytes().to_vec(),
                        (None, Some(encoded)) => BASE64
                            .decode(encoded)
                            .map_err(|error| Failed::from(error.to_string()))?,
                        _ => {
                            return Err(Failed::from("binary frame requires exactly one encoding"));
                        }
                    };
                    Frame::Binary {
                        data,
                        replay: *replay,
                    }
                }
                FrameRecord::Close { code, reason } => Frame::Close {
                    code: *code,
                    reason: reason.clone(),
                },
                FrameRecord::TcpClosed => Frame::TcpClosed,
            })
        })
        .collect()
}

fn echo_harness(step: &testkit::ws::StepRecord) -> Result<impl FnMut(WsCall) -> WsReply, Failed> {
    let handshake = step
        .connect
        .as_ref()
        .map(|connect| {
            let status = if connect.handshake.no_response {
                None
            } else {
                Some(
                    testkit::http::status_of(&connect.handshake.status_line)
                        .map_err(|error| Failed::from(error.to_string()))?,
                )
            };
            Ok::<_, Failed>(Handshake {
                status,
                headers: connect.handshake.headers.clone(),
                body: connect
                    .handshake
                    .body
                    .iter()
                    .flat_map(|text| text.bytes())
                    .collect(),
            })
        })
        .transpose()?;
    let response = step
        .http
        .as_ref()
        .map(|http| {
            let body = match (&http.response.body, &http.response.body_base64) {
                (Some(text), None) => text.as_bytes().to_vec(),
                (None, Some(encoded)) => BASE64
                    .decode(encoded)
                    .map_err(|error| Failed::from(error.to_string()))?,
                (None, None) => Vec::new(),
                (Some(_), Some(_)) => return Err(Failed::from("ambiguous response body")),
            };
            Ok(Response {
                status: testkit::http::status_of(&http.response.status_line)
                    .map_err(|error| Failed::from(error.to_string()))?,
                headers: http.response.headers.clone(),
                body,
            })
        })
        .transpose()?;
    let frames = step
        .frames
        .iter()
        .map(|(client, frames)| Ok((client.clone(), recorded_frames(frames)?)))
        .collect::<Result<std::collections::BTreeMap<_, _>, Failed>>()?;
    Ok(move |call| match call {
        WsCall::Upgrade(_) => match &handshake {
            Some(handshake) => WsReply::Handshake(handshake.clone()),
            None => WsReply::Done,
        },
        WsCall::Http(_) => match &response {
            Some(response) => WsReply::Http(response.clone()),
            None => WsReply::Done,
        },
        WsCall::Take { client } => match frames.get(&client) {
            Some(frames) => WsReply::Frames(frames.clone()),
            None => WsReply::Done,
        },
        WsCall::Sent(_) | WsCall::ClientClose { .. } => WsReply::Done,
    })
}

fn add_ws(trials: &mut Vec<Trial>, path: &Path) {
    match testkit::ws::steps(path) {
        Ok(steps) => {
            for step in steps {
                let trial = format!("ws {}", step.id);
                let masks = masks();
                trials.push(Trial::test(trial, move || {
                    let mut harness = echo_harness(&step.record)?;
                    must_pass(testkit::ws::replay(&step, &masks, &mut harness))
                }));
            }
        }
        Err(error) => trials.push(load_failed(format!("ws load {}", path.display()), error)),
    }
}

fn hook_outcome(session_key: &str, log_session: &str) -> HookOutcome {
    HookOutcome {
        exit: 0,
        stdout: String::new(),
        stderr: String::new(),
        posts: vec![HookPost {
            url: "/api/capture".to_string(),
            content_type: "application/json".to_string(),
            body: format!(
                "{{\"session_key\":\"{session_key}\",\"settings\":{{\"last_event\":\"UserPromptSubmit\",\"last_ingest_at\":\"2026-11-01T09:00:00.000Z\"}},\"messages\":[{{\"event_id\":\"{session_key}:u1\",\"metadata\":{{\"ingest_key\":\"1799999999999-abcdef123\"}}}}]}}"
            ),
        }],
        files: vec![HookFile {
            path: ".rivetos/capture.log".to_string(),
            mode: "644".to_string(),
            content: format!("{log_session} UserPromptSubmit 2026-11-01T09:00:00.123Z +1 msg\n"),
        }],
    }
}

fn hook_entries(path: &Path) -> Result<Vec<testkit::capture::EntryRecord>, Failed> {
    #[derive(serde::Deserialize)]
    struct Header {
        index: u32,
    }
    let text = std::fs::read_to_string(path).map_err(|error| Failed::from(error.to_string()))?;
    let rows: Vec<&serde_json::value::RawValue> =
        serde_json::from_str(&text).map_err(|error| Failed::from(error.to_string()))?;
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    for row in rows {
        let header: Header =
            serde_json::from_str(row.get()).map_err(|error| Failed::from(error.to_string()))?;
        if header.index == 1 {
            let skip: SkippedCase =
                serde_json::from_str(include_str!("fixtures/capture-skipped.json"))
                    .map_err(|error| Failed::from(error.to_string()))?;
            assert_eq!(skip.name, format!("hook/{}", header.index));
            assert_ne!(skip.reason.trim(), "");
            assert!(serde_json::from_str::<testkit::capture::EntryRecord>(row.get()).is_err());
            skipped.push(header.index);
        } else {
            entries.push(
                serde_json::from_str(row.get()).map_err(|error| Failed::from(error.to_string()))?,
            );
        }
    }
    assert_eq!(skipped, [1]);
    Ok(entries)
}

fn add_capture(trials: &mut Vec<Trial>, path: &Path) -> Result<(), Failed> {
    for entry in hook_entries(path)? {
        let trial = format!("capture hook/{}", entry.index);
        trials.push(Trial::test(trial, move || {
            let outcome = testkit::capture::replay(&entry, |_event, _stdin| {
                hook_outcome(
                    "claude-code:33333333-3333-3333-3333-333333333333",
                    "claude-code:33333333-3333-3333-3333-333333333333",
                )
            });
            must_pass(outcome)
        }));
    }
    Ok(())
}

fn pg_outcome(_call: &PgStep) -> PgOutcome {
    let conversation = "99999999-8888-4333-8444-555555555555";
    let message = "77777777-6666-4333-8444-555555555555";
    PgOutcome {
        result: Some(json!({
            "sessionKey": "claude-code:33333333-3333-3333-3333-333333333333",
            "conversationId": conversation,
            "created": true
        })),
        error: None,
        rows: Some(json!({
            "ros_conversations": [
                { "id": conversation, "session_key": "claude-code:33333333-3333-3333-3333-333333333333", "created_at": "2026-10-10 12:00:00+00" }
            ],
            "ros_messages": [
                { "id": message, "conversation_id": conversation, "created_at": "2026-10-12T10:15:30.123456+00:00" }
            ]
        })),
    }
}

fn add_capture_pg(trials: &mut Vec<Trial>, path: &Path) {
    match testkit::capture_pg::scenarios(path) {
        Ok(list) => {
            for scenario in list {
                let trial = format!("pg {}", scenario.name);
                trials.push(Trial::test(trial, move || {
                    must_pass(testkit::capture_pg::replay_scenario(
                        &scenario, None, pg_outcome,
                    ))
                }));
            }
        }
        Err(error) => trials.push(load_failed(format!("pg load {}", path.display()), error)),
    }
}

fn add_provider(trials: &mut Vec<Trial>) {
    let path = fixture("provider/A-text");
    match testkit::provider::load_scenario(&path) {
        Ok(scenario) => {
            let trial = format!("provider {}", scenario.name);
            trials.push(Trial::test(trial, move || {
                must_pass(testkit::provider::replay(&scenario, |canned| {
                    let status = canned.first().map(|c| c.status);
                    similar_asserts::assert_eq!(status, Some(200));
                    ProviderTurn {
                        requests: vec![testkit::CapturedRequest {
                            method: "POST".to_string(),
                            target: "/v1/chat/completions".to_string(),
                            headers: Vec::new(),
                            body: r#"{"stream":true,"messages":[{"content":"Say hi.","role":"user"}],"model":"test-model"}"#
                                .to_string(),
                        }],
                        chunks: vec![
                            json!({"text": "Hi", "type": "text"}),
                            json!({"type": "done"}),
                        ],
                    }
                }))
            }));
        }
        Err(error) => trials.push(load_failed(
            format!("provider load {}", path.display()),
            error,
        )),
    }
}

#[derive(serde::Deserialize)]
struct SkippedCase {
    name: String,
    reason: String,
}

fn add_cases(trials: &mut Vec<Trial>, path: &Path) -> Result<(), Failed> {
    let cases = testkit::cases::cases(path).map_err(|error| Failed::from(error.to_string()))?;
    let skips: Vec<SkippedCase> = serde_json::from_str(include_str!("fixtures/skipped.json"))
        .map_err(|error| Failed::from(error.to_string()))?;
    assert_eq!(cases.len(), skips.len());
    for (case, skip) in cases.into_iter().zip(skips) {
        assert_eq!(skip.name, format!("{}:{}", case.function, case.line));
        assert_ne!(skip.reason.trim(), "");
        trials.push(
            Trial::test(format!("case {}: {}", skip.name, skip.reason), || Ok(()))
                .with_ignored_flag(true),
        );
    }
    Ok(())
}

fn main() -> Result<(), Failed> {
    let args = Arguments::from_args();
    let mut trials = Vec::new();
    add_http(&mut trials, &fixture("http.json"))?;
    add_recipe(&mut trials, &fixture("recipe.json"));
    add_ws(&mut trials, &fixture("ws.json"));
    add_capture(&mut trials, &fixture("hook.json"))?;
    add_capture_pg(&mut trials, &fixture("pg.json"));
    add_provider(&mut trials);
    add_cases(&mut trials, &fixture("cases.jsonl"))?;
    drift::drift_http_status_and_headers(&mut trials);
    drift::add_ws_drift(&mut trials);
    drift::add_capture_drift(&mut trials);
    drift::add_pg_drift(&mut trials);
    drift::add_provider_drift(&mut trials);
    drift::add_provider_shared(&mut trials);
    drift::add_cases_drift(&mut trials);
    run(&args, trials).exit();
}
