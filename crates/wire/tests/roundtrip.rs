use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;

const GOLDEN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");

fn golden(rel: &str) -> PathBuf {
    Path::new(GOLDEN_DIR).join(rel)
}

fn recording(rel: &str) -> PathBuf {
    testkit::recording(rel)
}

fn load(rel: &str, path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{rel}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{rel}: {e}"))
}

const SKIPPED_CASES: &[&str] = &[
    "post-clamp-low",
    "post-clamp-nonnumber",
    "claim-a-sized",
    "garbage-ignored",
    "post-reserialize",
    "event-reserialize",
    "malformed-request-line",
];

fn strip_nulls(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, member| !member.is_null());
            for member in map.values_mut() {
                strip_nulls(member);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_nulls(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn roundtrip_as<T>(label: &str, raw: &str) -> Result<(), String>
where
    T: DeserializeOwned + Serialize,
{
    let parsed: Value = serde_json::from_str(raw).map_err(|e| format!("{label}: parse: {e}"))?;
    let typed: T =
        serde_json::from_value(parsed.clone()).map_err(|e| format!("{label}: type: {e}"))?;
    let out = serde_json::to_value(&typed).map_err(|e| format!("{label}: serialize: {e}"))?;
    let mut want = parsed;
    let mut got = out;
    strip_nulls(&mut want);
    strip_nulls(&mut got);
    if got != want {
        return Err(format!(
            "{label}: roundtrip mismatch\n  in : {want}\n  out: {got}"
        ));
    }
    Ok(())
}

fn request_head(head: &str) -> Option<(&str, &str)> {
    let first = head.split("\r\n").next()?;
    let mut parts = first.split(' ');
    let method = parts.next()?;
    let target = parts.next()?;
    Some((method, target))
}

fn path_of(target: &str) -> &str {
    target.split_once('?').map_or(target, |(path, _)| path)
}

fn status_of(status_line: &str) -> Option<u16> {
    status_line.split(' ').nth(1)?.parse().ok()
}

fn get<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

fn error_payload(label: &str, body: &str, parsed: &Value, errors: &mut Vec<String>) {
    if parsed.get("error").is_some() {
        errors.push_result(roundtrip_as::<wire::gateway::GatewayError>(label, body));
    }
}

fn success_payload(path: &str, body: &str, label: &str, errors: &mut Vec<String>) {
    if path == "/healthz" {
        errors.push_result(roundtrip_as::<wire::gateway::HealthzResponse>(label, body));
    }
}

fn response_payload(path: &str, status: u16, body: &str, errors: &mut Vec<String>) {
    let Ok(parsed) = serde_json::from_str::<Value>(body) else {
        return;
    };
    let label = format!("{status} {path}");
    if status >= 400 {
        error_payload(&label, body, &parsed, errors);
    } else {
        success_payload(path, body, &label, errors);
    }
}

fn roundtrip_case(case: &Value, errors: &mut Vec<String>) {
    let id = case.get("id").and_then(Value::as_str).unwrap_or_default();
    if SKIPPED_CASES.contains(&id) {
        return;
    }
    let Some(head) = get(case, "request").get("head").and_then(Value::as_str) else {
        return;
    };
    let Some((_, target)) = request_head(head) else {
        errors.push("the recorded request head has no method and target".to_owned());
        return;
    };
    let path = path_of(target);
    let response = get(case, "response");
    let Some(status) = status_of(
        response
            .get("statusLine")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    ) else {
        errors.push("the recorded status line carries no status code".to_owned());
        return;
    };
    if let Some(body) = response.get("body").and_then(Value::as_str) {
        response_payload(path, status, body, errors);
    }
}

fn roundtrip_exchange_file(rel: &str, path: &Path, errors: &mut Vec<String>) -> Result<(), String> {
    let doc = load(rel, path)?;
    let Some(runs) = doc.get("runs").and_then(Value::as_object) else {
        return Ok(());
    };
    for (run_name, run) in runs {
        let Some(cases) = run.get("cases").and_then(Value::as_array) else {
            continue;
        };
        for case in cases {
            let id = case.get("id").and_then(Value::as_str).unwrap_or_default();
            let before = errors.len();
            roundtrip_case(case, errors);
            for failure in errors.iter_mut().skip(before) {
                *failure = format!("{rel} {run_name} {id}: {failure}");
            }
        }
    }
    Ok(())
}

fn ws_payload(text: &str, errors: &mut Vec<String>) {
    let Ok(parsed) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let kind = parsed
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let label = format!("ws frame {kind}");
    match kind {
        "hub.state" | "plugin.changed" | "plugin.views" | "topic.changed" => {
            errors.push_result(roundtrip_as::<wire::events::SessionBroadcast>(&label, text));
        }
        _ => {}
    }
}

fn roundtrip_ws_file(rel: &str, path: &Path, errors: &mut Vec<String>) -> Result<(), String> {
    let doc = load(rel, path)?;
    let Some(runs) = doc.get("runs").and_then(Value::as_object) else {
        return Ok(());
    };
    for (run_name, run) in runs {
        let Some(steps) = run.get("steps").and_then(Value::as_array) else {
            continue;
        };
        for step in steps {
            let id = step.get("id").and_then(Value::as_str).unwrap_or_default();
            if SKIPPED_CASES.contains(&id) {
                continue;
            }
            let before = errors.len();
            if step.get("http").is_some_and(Value::is_object) {
                roundtrip_case(get(step, "http"), errors);
            }
            let text_frames = |frame: &Value, errors: &mut Vec<String>| {
                if frame.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(text) = frame.get("text").and_then(Value::as_str)
                {
                    ws_payload(text, errors);
                }
            };
            for frame in step
                .get("sent")
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new())
            {
                text_frames(frame, errors);
            }
            if let Some(frames) = step.get("frames").and_then(Value::as_object) {
                for client_frames in frames.values() {
                    for frame in client_frames.as_array().unwrap_or(&Vec::new()) {
                        text_frames(frame, errors);
                    }
                }
            }
            for failure in errors.iter_mut().skip(before) {
                *failure = format!("{rel} {run_name} {id}: {failure}");
            }
        }
    }
    Ok(())
}

fn roundtrip_capture_batches(rel: &str, errors: &mut Vec<String>) -> Result<(), String> {
    let doc = load(rel, &recording(rel))?;
    let Some(entries) = doc.as_array() else {
        return Ok(());
    };
    for entry in entries {
        let Some(body) = entry.get("body").and_then(Value::as_str) else {
            continue;
        };
        if entry.get("url").and_then(Value::as_str) != Some("/api/capture") {
            continue;
        }
        let before = errors.len();
        errors.push_result(roundtrip_as::<wire::capture::CaptureBatchRequest>(
            &format!("{rel} batch"),
            body,
        ));
        for failure in errors.iter_mut().skip(before) {
            *failure = format!("{rel}: {failure}");
        }
    }
    Ok(())
}

fn roundtrip_capture_results(errors: &mut Vec<String>) -> Result<(), String> {
    let rel = "capture/writer/results.json";
    let doc = load(rel, &recording(rel))?;
    let Some(results) = doc.as_object() else {
        return Ok(());
    };
    for result in results.values() {
        let body = result.to_string();
        errors.push_result(roundtrip_as::<wire::capture::CaptureResult>(rel, &body));
    }
    Ok(())
}

fn provider_chat_request(label: &str, body: &str) -> Result<(), String> {
    let mut payload: Value =
        serde_json::from_str(body).map_err(|e| format!("{label}: parse: {e}"))?;
    let Some(object) = payload.as_object_mut() else {
        return Ok(());
    };
    for (field, _) in testkit::provider::B8_OMITTED_REQUEST_FIELDS {
        object.remove(*field);
    }
    roundtrip_as::<wire::stream::openai::ChatCompletionRequest>(label, &payload.to_string())
}

fn provider_requests(server: &str, scenario: &str, errors: &mut Vec<String>) -> Result<(), String> {
    let rel = format!("providers/{server}/{scenario}/requests.json");
    let doc = load(&rel, &recording(&rel))?;
    let Some(entries) = doc.as_array() else {
        return Ok(());
    };
    for entry in entries {
        let Some(body) = entry.get("body").and_then(Value::as_str) else {
            continue;
        };
        let target = entry
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let before = errors.len();
        if target == "/v1/chat/completions" {
            errors.push_result(provider_chat_request(&format!("{rel} {target}"), body));
        }
        for failure in errors.iter_mut().skip(before) {
            *failure = format!("{rel}: {failure}");
        }
    }
    Ok(())
}

fn typed_chunk(label: &str, payload: &str) -> Result<(), String> {
    roundtrip_as::<wire::stream::openai::ChatCompletionChunk>(label, payload)
}

fn provider_responses(
    server: &str,
    scenario: &str,
    errors: &mut Vec<String>,
) -> Result<(), String> {
    let dir = format!("providers/{server}/{scenario}");
    let dir_path = recording(&dir);
    let entries = std::fs::read_dir(&dir_path).map_err(|e| format!("{dir}: {e}"))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_response = name.starts_with("response-")
            && name
                .rsplit('.')
                .next()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"));
        if !is_response {
            continue;
        }
        let rel = format!("{dir}/{name}");
        let text =
            std::fs::read_to_string(dir_path.join(&name)).map_err(|e| format!("{rel}: {e}"))?;
        if text.trim_start().starts_with('{') {
            let before = errors.len();
            errors.push_result(roundtrip_as::<wire::stream::openai::ModelList>(
                &format!("{rel} body"),
                text.trim_start(),
            ));
            for failure in errors.iter_mut().skip(before) {
                *failure = format!("{rel}: {failure}");
            }
            continue;
        }
        for line in text.lines() {
            let Some(payload) = line
                .trim()
                .strip_prefix("data: ")
                .map(str::trim)
                .filter(|p| !p.is_empty() && *p != "[DONE]")
            else {
                continue;
            };
            let before = errors.len();
            errors.push_result(typed_chunk(&format!("{rel} chunk"), payload));
            for failure in errors.iter_mut().skip(before) {
                *failure = format!("{rel}: {failure}");
            }
        }
    }
    Ok(())
}

fn provider_chunks(server: &str, scenario: &str, errors: &mut Vec<String>) {
    let rel = format!("providers/{server}/{scenario}/chunks.json");
    let Ok(doc) = load(&rel, &golden(&rel)) else {
        return;
    };
    let Some(entries) = doc.as_array() else {
        return;
    };
    for entry in entries {
        let body = entry.to_string();
        let before = errors.len();
        errors.push_result(roundtrip_as::<wire::stream::LlmChunk>(
            &format!("{rel} chunk"),
            &body,
        ));
        for failure in errors.iter_mut().skip(before) {
            *failure = format!("{rel}: {failure}");
        }
    }
}

trait PushResult {
    fn push_result(&mut self, result: Result<(), String>);
}

impl PushResult for Vec<String> {
    fn push_result(&mut self, result: Result<(), String>) {
        if let Err(e) = result {
            self.push(e);
        }
    }
}

fn check(errors: &[String]) -> Result<(), String> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} failure(s):\n{}",
            errors.len(),
            errors.join("\n")
        ))
    }
}

#[test]
fn gateway_http_bodies_roundtrip() -> Result<(), String> {
    let mut errors = Vec::new();
    roundtrip_exchange_file(
        "gateway/http-plain.json",
        &recording("gateway/http-plain.json"),
        &mut errors,
    )?;
    roundtrip_exchange_file(
        "gateway/http-tls.json",
        &golden("gateway/http-tls.json"),
        &mut errors,
    )?;
    check(&errors)
}

#[test]
fn share_report_roundtrips() -> Result<(), String> {
    let rel = "gateway/share-report.json";
    let text = std::fs::read_to_string(golden(rel)).map_err(|e| format!("{rel}: {e}"))?;
    roundtrip_as::<wire::gateway::share::DraftReport>(rel, &text)
}

#[test]
fn gateway_ws_frames_roundtrip() -> Result<(), String> {
    let mut errors = Vec::new();
    roundtrip_ws_file("gateway/ws.json", &golden("gateway/ws.json"), &mut errors)?;
    check(&errors)
}

#[test]
fn term_http_bodies_roundtrip() -> Result<(), String> {
    let mut errors = Vec::new();
    roundtrip_exchange_file("term/http.json", &golden("term/http.json"), &mut errors)?;
    check(&errors)
}

#[test]
fn term_ws_frames_roundtrip() -> Result<(), String> {
    let mut errors = Vec::new();
    roundtrip_ws_file("term/ws.json", &golden("term/ws.json"), &mut errors)?;
    check(&errors)
}

#[test]
fn capture_batches_roundtrip() -> Result<(), String> {
    let mut errors = Vec::new();
    for name in [
        "capture/writer/single.input.json",
        "capture/writer/numbers_and_keys.input.json",
        "capture/writer/redaction.input.json",
        "capture/writer/surrogate_cap.input.json",
        "capture/writer/finalize_multi_chunk.input.json",
        "capture/writer/elide_tool_args.input.json",
    ] {
        let body = load(name, &recording(name))?;
        errors.push_result(roundtrip_as::<wire::capture::CaptureBatchRequest>(
            name,
            &body.to_string(),
        ));
    }
    for name in [
        "capture/writer/single.posts.json",
        "capture/writer/numbers_and_keys.posts.json",
        "capture/writer/redaction.posts.json",
        "capture/writer/surrogate_cap.posts.json",
        "capture/writer/finalize_multi_chunk.posts.json",
        "capture/writer/elide_tool_args.posts.json",
        "capture/claude-hook/hook-rust.json",
        "capture/claude-hook/hook-ts.json",
    ] {
        roundtrip_capture_batches(name, &mut errors)?;
    }
    roundtrip_capture_results(&mut errors)?;
    check(&errors)
}

#[test]
fn provider_payloads_roundtrip() -> Result<(), String> {
    let mut errors = Vec::new();
    let server = "vllm";
    for scenario in [
        "A-text",
        "B-tool",
        "C-reasoning",
        "D-thinking-off",
        "E-reasoning-tool",
        "probe",
    ] {
        provider_requests(server, scenario, &mut errors)?;
        provider_responses(server, scenario, &mut errors)?;
        provider_chunks(server, scenario, &mut errors);
    }
    check(&errors)
}

#[test]
fn llama_cpp_timings_chunk_roundtrips() -> Result<(), String> {
    let chunk = r#"{"choices":[],"created":1793419284,"id":"chatcmpl-0SKSM8pfY3wNH8bLDE06FtAWSWu2XTQL","model":"hy-memory","system_fingerprint":"b50-1052f7e","object":"chat.completion.chunk","usage":{"completion_tokens":5,"prompt_tokens":17,"total_tokens":22,"prompt_tokens_details":{"cached_tokens":0}},"timings":{"cache_n":0,"prompt_n":17,"prompt_ms":46.54,"prompt_per_token_ms":2.7376470588235295,"prompt_per_second":365.2771809196391,"predicted_n":5,"predicted_ms":40.337,"predicted_per_token_ms":10.08425,"predicted_per_second":99.16453876093908}}"#;
    typed_chunk("llama.cpp timings chunk", chunk)
}

#[test]
fn users_registry_fixtures_roundtrip() -> Result<(), String> {
    let mut errors = Vec::new();
    let tenancy = load(
        "gateway/users-tenancy.json",
        &golden("gateway/users-tenancy.json"),
    )?;
    errors.push_result(roundtrip_as::<wire::gateway::users::UsersRegistry>(
        "gateway/users-tenancy.json",
        &tenancy.to_string(),
    ));
    let registry: wire::gateway::users::UsersRegistry =
        serde_json::from_value(tenancy).map_err(|e| format!("gateway/users-tenancy.json: {e}"))?;
    let alice = registry.users.get("alice").ok_or("alice missing")?;
    assert_eq!(
        alice.persona.clone().ok_or("alice persona missing")?,
        "alice"
    );
    let renamed = registry.users.get("user-b").ok_or("user-b missing")?;
    assert_eq!(renamed.pg_url, None);
    assert_eq!(
        renamed.devices.clone().ok_or("user-b devices missing")?,
        vec!["device:nodb".to_string()]
    );
    let invalid = std::fs::read_to_string(format!("{GOLDEN_DIR}/gateway/users-invalid.json"))
        .map_err(|e| format!("gateway/users-invalid.json: {e}"))?;
    if serde_json::from_str::<wire::gateway::users::UsersRegistry>(&invalid).is_ok() {
        errors.push("gateway/users-invalid.json: truncated registry must not parse".to_string());
    }
    check(&errors)
}
