use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::HeaderName;
use axum::http::HeaderValue;
use axum::http::Method;
use axum::http::Request;
use axum::http::Uri;
use chrono::DateTime;
use chrono::Utc;
use gateway::Settings;
use gateway::tenancy::Tenancy;
use gateway::tenancy::UserId;
use serde::Deserialize;
use testkit::http::HttpCase;
use tower::ServiceExt;
use wire::events::Activity;
use wire::events::AgentEvent;
use wire::events::AgentEventKind;
use wire::events::ProtocolVersion;

const OWN: &[&str] = &[
    "plain-failclosed/failclosed-healthz",
    "plain-failclosed/failclosed-options",
    "plain-failclosed/failclosed-static",
    "plain-main/alias-api-events",
    "plain-main/alias-api-events-event",
    "plain-main/alias-api-events-sessions",
    "plain-main/alias-api-events-state",
    "plain-main/alias-api-terminal",
    "plain-main/alias-api-terminal-config",
    "plain-main/alias-api-terminal-harness-sessions",
    "plain-main/alias-api-terminal-inject",
    "plain-main/alias-api-terminal-list",
    "plain-main/alias-api-terminal-prefix",
    "plain-main/delete-session-again",
    "plain-main/delete-session-canonical",
    "plain-main/delete-session-known",
    "plain-main/delete-session-missing-param",
    "plain-main/event-canonical-uuid-room",
    "plain-main/event-empty-body",
    "plain-main/event-get",
    "plain-main/event-invalid-json",
    "plain-main/event-valid-chunked",
    "plain-main/event-valid-expect-continue",
    "plain-main/event-valid-session-start",
    "plain-main/events-100",
    "plain-main/events-101",
    "plain-main/events-bad-element-2",
    "plain-main/events-empty-array",
    "plain-main/events-not-array",
    "plain-main/events-valid-batch",
    "plain-main/host-allowed-env",
    "plain-main/host-dot-localhost",
    "plain-main/host-empty",
    "plain-main/host-evil",
    "plain-main/host-evil-with-port",
    "plain-main/host-ipv6-loopback",
    "plain-main/host-localhost-upper",
    "plain-main/healthz-after-ingest",
    "plain-main/host-rebinding-same-origin",
    "plain-main/origin-allowed-env",
    "plain-main/origin-allowed-env-default-port",
    "plain-main/origin-app-bundle",
    "plain-main/origin-app-bundle-upper",
    "plain-main/origin-default-port",
    "plain-main/origin-disallowed",
    "plain-main/origin-disallowed-repeat",
    "plain-main/origin-empty",
    "plain-main/origin-mesh-peer-explicit-scheme-only",
    "plain-main/origin-none",
    "plain-main/origin-null",
    "plain-main/origin-port-mismatch",
    "plain-main/origin-same",
    "plain-main/origin-same-other-scheme",
    "plain-main/origin-same-uppercase",
    "plain-main/origin-same-with-path",
    "plain-main/origin-unparseable",
    "plain-main/remote-plain-api",
    "plain-main/session-get",
    "plain-main/sessions-after-delete",
    "plain-main/sessions-list",
    "plain-main/state-after-rejected-batch",
    "plain-main/state-canonical-id",
    "plain-main/state-duplicate-param",
    "plain-main/state-empty-param",
    "plain-main/state-known",
    "plain-main/state-missing-param",
    "plain-main/state-post",
    "plain-main/state-reserialized",
    "plain-main/state-unknown",
    "plain-main/static-term-nested",
    "plain-main/term-config",
    "plain-main/term-post-disabled",
    "plain-nostatic/nostatic-ws-get",
    "plain-tenancy/tenancy-healthz",
    "plain-tenancy/tenancy-ingest-alice-session",
    "plain-tenancy/tenancy-ingest-owner-session",
    "plain-tenancy/tenancy-sessions-owner",
    "plain-tenancy/tenancy-state-own",
    "plain-tenancy/tenancy-state-unknown",
    "plain-tenancy/tenancy-static",
    "tls-main/tls-api-no-cert",
    "tls-main/tls-api-node-leaf",
    "tls-main/tls-api-path-html-accept",
    "tls-main/tls-api-serverauth-only",
    "tls-main/tls-api-wrong-ca",
    "tls-main/tls-enroll-no-cert",
    "tls-main/tls-healthz-no-cert",
    "tls-main/tls-loopback-ingest",
    "tls-main/tls-loopback-sessions",
    "tls-main/tls-options-no-cert",
    "tls-main/tls-origin-disallowed",
    "tls-main/tls-post-no-cert-html",
    "tls-main/tls-root-no-cert-json",
    "tls-optional/tls-optional-healthz",
    "tls-tenancy/tt-alice-healthz",
    "tls-tenancy/tt-ingest-alice-session",
    "tls-tenancy/tt-ingest-owner-session",
    "tls-tenancy/tt-no-cert",
    "tls-tenancy/tt-owner-loopback-sessions",
];

const BATCH_ORIGIN_MS: i64 = 1_760_000_100_000;
const RESPONSE_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
struct ChunkedDoc {
    #[serde(default)]
    runs: BTreeMap<String, ChunkedRun>,
}

#[derive(Deserialize)]
struct ChunkedRun {
    #[serde(default)]
    cases: Vec<ChunkedCase>,
}

#[derive(Deserialize)]
struct ChunkedCase {
    #[serde(default)]
    id: String,
    #[serde(default)]
    request: ChunkedRequest,
}

#[derive(Deserialize, Default)]
struct ChunkedRequest {
    #[serde(rename = "chunkedBody", default)]
    chunked_body: Vec<String>,
}

fn masks() -> testkit::Masks {
    testkit::Masks {
        port: "5174".to_owned(),
        hostname: Some("gold-host".to_owned()),
        tmp: Some("/tmp/golden".to_owned()),
    }
}

fn settings() -> Settings {
    let scratch = PathBuf::from("/tmp/rr-gateway-unused-agents");
    Settings {
        hostname: "gold-host".to_owned(),
        node: "golden-node".to_owned(),
        allowed_origins: vec!["https://hub.example.test".to_owned()],
        allowed_hosts: vec!["den.example.test".to_owned()],
        files_root: None,
        static_dir: None,
        root_redirect: None,
        state_dir: scratch.join("state"),
        directory_root: scratch.join("agents"),
        shared_dir: scratch.join("shared"),
        allowed_harnesses: None,
        probe_timeout: Duration::from_millis(200),
        private_ca: None,
    }
}

const INDEX: &str = "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>golden hub</title></head><body>golden index</body></html>\n";

fn static_root() -> Result<tempfile::TempDir, String> {
    let root = tempfile::tempdir().map_err(|error| format!("static dir: {error}"))?;
    std::fs::write(root.path().join("index.html"), INDEX)
        .map_err(|error| format!("index.html: {error}"))?;
    Ok(root)
}

fn app(run: &str, static_dir: Option<&Path>) -> Result<axum::Router, String> {
    let owner = UserId::new("owner".to_owned()).map_err(|error| format!("owner: {error}"))?;
    let store = Arc::new(testkit::FakeMemoryStore::default());
    let handle: Arc<dyn kernel::memory::MemoryStore> = store;
    let mut configured = settings();
    if let Some(dir) = static_dir {
        configured.static_dir = Some(dir.to_path_buf());
    }
    let gateway =
        Arc::new(gateway::Gateway::new(handle, configured).with_tenancy(Tenancy::local(owner)));
    for (session, name) in seeded_owners(run)? {
        let user = UserId::new(name).map_err(|error| format!("seed user: {error}"))?;
        gateway
            .sessions()
            .seed_owner(&user, &session)
            .map_err(|error| format!("seed {session}: {error}"))?;
    }
    Ok(gateway.router(false))
}

#[derive(Deserialize)]
struct OwnerDoc {
    runs: BTreeMap<String, OwnerRun>,
}

#[derive(Deserialize)]
struct OwnerRun {
    #[serde(rename = "stateAfter", default)]
    state_after: BTreeMap<String, Option<String>>,
}

fn seeded_owners(run: &str) -> Result<Vec<(String, String)>, String> {
    for recording in http_paths() {
        let text = std::fs::read_to_string(&recording)
            .map_err(|error| format!("read {}: {error}", recording.display()))?;
        let doc: OwnerDoc = serde_json::from_str(&text)
            .map_err(|error| format!("owners {}: {error}", recording.display()))?;
        let Some(entry) = doc.runs.get(run) else {
            continue;
        };
        let Some(raw) = entry
            .state_after
            .get("session-owners.json")
            .and_then(Option::as_ref)
        else {
            return Ok(Vec::new());
        };
        let owners: BTreeMap<String, String> =
            serde_json::from_str(raw).map_err(|error| format!("session owners {run}: {error}"))?;
        return Ok(owners.into_iter().collect());
    }
    Ok(Vec::new())
}

fn reserialized_start() -> Result<String, String> {
    let ts = DateTime::<Utc>::from_timestamp_millis(1_760_000_004_500)
        .ok_or_else(|| "golden-raw timestamp".to_owned())?;
    let event = AgentEvent {
        v: ProtocolVersion::V1,
        session: "golden-raw".to_owned(),
        kind: AgentEventKind::SessionStart {
            title: Some("café 😀 line\u{2028}sep".to_owned()),
            name: Some("dup-final".to_owned()),
            harness: Some(wire::harness::SessionHarness::Rivetos),
            ts: Some(ts),
        },
        extra: wire::Extra::default(),
    };
    serde_json::to_string(&event).map_err(|error| format!("golden-raw event: {error}"))
}

async fn apply_reserialized(app: &axum::Router) -> Result<(), String> {
    let request = testkit::Request {
        method: "POST".to_owned(),
        target: "/event".to_owned(),
        headers: vec![
            ("Host".to_owned(), "127.0.0.1:5174".to_owned()),
            ("Content-Type".to_owned(), "application/json".to_owned()),
        ],
        body: reserialized_start()?.into_bytes(),
        recipe: None,
        via: Some("loopback (source 127.0.0.1)".to_owned()),
    };
    let actual = answer(app.clone(), request).await?;
    if actual.status == 200 {
        Ok(())
    } else {
        let body = String::from_utf8_lossy(&actual.body);
        Err(format!("status {} {body}", actual.status))
    }
}

fn activity_event(index: i64) -> Result<AgentEvent, String> {
    let millis = BATCH_ORIGIN_MS
        .checked_add(index)
        .ok_or_else(|| format!("timestamp {index}"))?;
    let ts = DateTime::<Utc>::from_timestamp_millis(millis)
        .ok_or_else(|| format!("timestamp {millis}"))?;
    let activity = if index % 2 == 1 {
        Activity::Thinking
    } else {
        Activity::Idle
    };
    Ok(AgentEvent {
        v: ProtocolVersion::V1,
        session: "golden-batch".to_owned(),
        kind: AgentEventKind::Activity {
            activity,
            ts: Some(ts),
        },
        extra: wire::Extra::default(),
    })
}

fn activity_batch(count: i64) -> Result<String, String> {
    let mut events = Vec::new();
    for index in 0..count {
        events.push(activity_event(index)?);
    }
    serde_json::to_string(&events).map_err(|error| format!("activity batch: {error}"))
}

fn activity_count(recipe: &str) -> Result<i64, String> {
    let Some((count, _)) = recipe.split_once(' ') else {
        return Err(format!("activity recipe {recipe}"));
    };
    count
        .parse()
        .map_err(|error| format!("activity count: {error}"))
}

fn chunked_bodies(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    let doc: ChunkedDoc = serde_json::from_str(&text)
        .map_err(|error| format!("chunks {}: {error}", path.display()))?;
    let mut joined = BTreeMap::new();
    for (run, entry) in doc.runs {
        for case in entry.cases {
            if case.request.chunked_body.is_empty() {
                continue;
            }
            joined.insert(
                format!("{run}/{}", case.id),
                case.request.chunked_body.join(""),
            );
        }
    }
    Ok(joined)
}

fn prepare(case: &HttpCase, chunks: &BTreeMap<String, String>) -> Result<HttpCase, String> {
    let mut prepared = case.clone();
    if let Some(joined) = chunks.get(&prepared.id) {
        prepared.request.body = Some(joined.clone());
    }
    let Some(recipe) = prepared.request.body_recipe.clone() else {
        return Ok(prepared);
    };
    if !recipe.contains("activity") {
        return Err(format!("{} recipe {recipe}", prepared.id));
    }
    prepared.request.body = Some(activity_batch(activity_count(&recipe)?)?);
    prepared.request.body_recipe = None;
    Ok(prepared)
}

fn recorded_request(case: &HttpCase) -> Result<testkit::Request, String> {
    let substituted = testkit::substitute(&case.request.head, &masks());
    let Some(head) = testkit::http::parse_head(&substituted) else {
        return Err(format!("{} request head", case.id));
    };
    let body = match case.request.body.as_deref() {
        Some(text) => testkit::substitute(text, &masks()).into_bytes(),
        None => Vec::new(),
    };
    let recipe = case
        .request
        .body_recipe
        .as_deref()
        .map(|text| testkit::substitute(text, &masks()));
    Ok(testkit::Request {
        method: head.method,
        target: head.target,
        headers: head.headers,
        body,
        recipe,
        via: case.request.via.clone(),
    })
}

fn peer_addr(recorded: &testkit::Request) -> Result<SocketAddr, String> {
    let remote = match recorded.via.as_deref() {
        Some(via) => via.contains("remote") || via.contains("127.0.0.2"),
        None => false,
    };
    let text = if remote {
        "127.0.0.2:41000"
    } else {
        "127.0.0.1:41000"
    };
    text.parse()
        .map_err(|error| format!("peer address: {error}"))
}

fn http_request(recorded: testkit::Request) -> Result<Request<Body>, String> {
    let addr = peer_addr(&recorded)?;
    let uri: Uri = recorded
        .target
        .parse()
        .map_err(|error| format!("recorded target {}: {error}", recorded.target))?;
    let method = Method::from_bytes(recorded.method.as_bytes())
        .map_err(|error| format!("recorded method: {error}"))?;
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in &recorded.headers {
        let parsed_name = HeaderName::from_bytes(name.to_ascii_lowercase().as_bytes())
            .map_err(|error| format!("header name {name}: {error}"))?;
        let parsed_value = HeaderValue::from_bytes(value.as_bytes())
            .map_err(|error| format!("header value {value}: {error}"))?;
        builder = builder.header(parsed_name, parsed_value);
    }
    let mut request = builder
        .body(Body::from(recorded.body))
        .map_err(|error| format!("recorded request: {error}"))?;
    request.extensions_mut().insert(ConnectInfo(addr));
    Ok(request)
}

async fn read_response(response: axum::response::Response) -> Result<testkit::Response, String> {
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let body = axum::body::to_bytes(response.into_body(), RESPONSE_LIMIT)
        .await
        .map_err(|error| format!("response body: {error}"))?
        .to_vec();
    Ok(testkit::Response {
        status,
        headers,
        body,
    })
}

async fn answer(
    app: axum::Router,
    recorded: testkit::Request,
) -> Result<testkit::Response, String> {
    let response = app
        .oneshot(http_request(recorded)?)
        .await
        .map_err(|error| format!("in-process service: {error}"))?;
    read_response(response).await
}

async fn play(
    case: &HttpCase,
    chunks: &BTreeMap<String, String>,
    app: &axum::Router,
) -> Result<(), String> {
    let prepared = prepare(case, chunks)?;
    let request = recorded_request(&prepared)?;
    let actual = answer(app.clone(), request).await?;
    testkit::http::replay(&prepared, &masks(), |_| actual).map_err(|mismatch| mismatch.detail)
}

fn runs_of(cases: &[HttpCase]) -> Result<Vec<String>, String> {
    let mut runs = Vec::new();
    for case in cases {
        let Some((run, _)) = case.id.split_once('/') else {
            return Err(format!("bare id {}", case.id));
        };
        if runs.last().is_none_or(|seen| seen != run) {
            runs.push(run.to_owned());
        }
    }
    Ok(runs)
}

fn run_has_owned(run: &str, cases: &[HttpCase], owned: &BTreeSet<&str>) -> bool {
    let prefix = format!("{run}/");
    cases
        .iter()
        .any(|case| case.id.starts_with(&prefix) && owned.contains(case.id.as_str()))
}

struct Lane<'a> {
    static_root: Option<&'a Path>,
    seen: &'a mut BTreeSet<String>,
    failures: &'a mut Vec<String>,
}

async fn replay_run(
    run: &str,
    cases: &[HttpCase],
    chunks: &BTreeMap<String, String>,
    owned: &BTreeSet<&str>,
    lane: &mut Lane<'_>,
) {
    let prefix = format!("{run}/");
    let app = match app(run, served_static(run, lane.static_root)) {
        Ok(app) => app,
        Err(error) => {
            lane.failures.push(format!("app {run}: {error}"));
            return;
        }
    };
    for case in cases {
        if !case.id.starts_with(&prefix) {
            continue;
        }
        if case.id == "plain-main/event-reserialize" {
            if let Err(detail) = apply_reserialized(&app).await {
                lane.failures.push(format!("{} :: {detail}", case.id));
            }
            continue;
        }
        if !owned.contains(case.id.as_str()) {
            continue;
        }
        if !lane.seen.insert(case.id.clone()) {
            lane.failures.push(format!("replayed twice {}", case.id));
            continue;
        }
        if let Err(detail) = play(case, chunks, &app).await {
            lane.failures
                .push(format!("{} :: {}", case.id, detail.replace('\n', " | ")));
        }
    }
}

fn http_paths() -> [PathBuf; 2] {
    [
        testkit::recording("gateway/http-plain.json"),
        testkit::golden_path(env!("CARGO_MANIFEST_DIR"), "../wire/tests/golden/gateway")
            .join("http-tls.json"),
    ]
}

fn served_static<'a>(run: &str, root: Option<&'a Path>) -> Option<&'a Path> {
    match run {
        "plain-failclosed" | "plain-tenancy" => root,
        _ => None,
    }
}

async fn replay_recording(path: &Path, owned: &BTreeSet<&str>, lane: &mut Lane<'_>) {
    let cases = match testkit::http::cases(path) {
        Ok(cases) => cases,
        Err(error) => {
            lane.failures.push(format!("{}: {error}", path.display()));
            return;
        }
    };
    let chunks = match chunked_bodies(path) {
        Ok(chunks) => chunks,
        Err(error) => {
            lane.failures.push(error);
            return;
        }
    };
    let runs = match runs_of(&cases) {
        Ok(runs) => runs,
        Err(error) => {
            lane.failures.push(error);
            return;
        }
    };
    for run in &runs {
        if !run_has_owned(run, &cases, owned) {
            continue;
        }
        replay_run(run, &cases, &chunks, owned, lane).await;
    }
}

fn finish(
    owned: &BTreeSet<&str>,
    seen: &BTreeSet<String>,
    mut failures: Vec<String>,
) -> Result<(), String> {
    for id in owned {
        if !seen.contains(*id) {
            failures.push(format!("missing recording {id}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

async fn replay_all() -> Result<(), String> {
    let owned: BTreeSet<&str> = OWN.iter().copied().collect();
    let mut seen = BTreeSet::new();
    let mut failures = Vec::new();
    let static_dir = static_root()?;
    {
        let mut lane = Lane {
            static_root: Some(static_dir.path()),
            seen: &mut seen,
            failures: &mut failures,
        };
        for path in http_paths() {
            replay_recording(&path, &owned, &mut lane).await;
        }
    }
    finish(&owned, &seen, failures)
}

#[tokio::test]
async fn replay_owned_http() {
    if let Err(problems) = replay_all().await {
        panic!("{problems}");
    }
}
