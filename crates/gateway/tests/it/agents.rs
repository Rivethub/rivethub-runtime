use crate::common;

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use gateway::Gateway;
use gateway::Settings;
use gateway::tenancy::{Tenancy, UserId};
use kernel::memory::MemoryStore;
use testkit::FakeMemoryStore;
use testkit::Request;
use wire::gateway::GatewayError;
use wire::gateway::OkReply;
use wire::gateway::agents::AgentListResponse;
use wire::gateway::agents::AgentPreset;
use wire::gateway::agents::AgentPresetCreateRequest;
use wire::gateway::agents::AgentPresetUpdate;
use wire::gateway::agents::AgentResponse;
use wire::gateway::agents::AgentStoreBackend;
use wire::gateway::agents::AgentsFile;
use wire::harness::HarnessId;

const NODE: &str = "golden-node";

struct Sandbox {
    app: Router,
    root: tempfile::TempDir,
}

impl Sandbox {
    #[expect(
        clippy::expect_used,
        reason = "a helper outside #[test] still has to fail the test when the sandbox directory cannot be created"
    )]
    fn open(allowed: Option<Vec<HarnessId>>) -> Self {
        let root = tempfile::tempdir().expect("sandbox directory");
        let gateway = gateway_at(root.path(), allowed);
        let app = gateway.router(false);
        Self { app, root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }
}

#[expect(
    clippy::expect_used,
    reason = "a helper outside #[test] still has to fail the test when the owner id is refused"
)]
fn gateway_at(root: &Path, allowed: Option<Vec<HarnessId>>) -> Arc<Gateway> {
    let store = Arc::new(FakeMemoryStore::default());
    let handle: Arc<dyn MemoryStore> = store;
    Arc::new(
        Gateway::new(
            handle,
            Settings {
                hostname: "gold-host".to_owned(),
                node: NODE.to_owned(),
                allowed_origins: vec!["https://hub.example.test".to_owned()],
                allowed_hosts: vec!["den.example.test".to_owned()],
                files_root: None,
                static_dir: None,
                root_redirect: None,
                state_dir: root.join("state"),
                directory_root: root.join("agents"),
                shared_dir: root.join("shared"),
                allowed_harnesses: allowed,
                probe_timeout: Duration::from_millis(200),
                private_ca: None,
            },
        )
        .with_tenancy(Tenancy::local(
            UserId::new("owner".to_owned()).expect("owner id"),
        )),
    )
}

#[expect(
    clippy::expect_used,
    reason = "a helper outside #[test] still has to fail the test when the route does not answer"
)]
async fn exchange(
    app: &Router,
    method: &str,
    target: &str,
    body: &[u8],
    remote: bool,
) -> testkit::Response {
    let request = Request {
        method: method.to_owned(),
        target: target.to_owned(),
        headers: vec![("Host".to_owned(), "127.0.0.1:5174".to_owned())],
        body: body.to_vec(),
        recipe: None,
        via: remote.then(|| "remote".to_owned()),
    };
    tokio::time::timeout(Duration::from_secs(5), common::answer(app.clone(), request))
        .await
        .expect("agent route timed out")
        .expect("agent route request")
}

#[expect(
    clippy::expect_used,
    reason = "a helper outside #[test] still has to fail the test when the request body cannot be encoded"
)]
async fn send_json(
    app: &Router,
    method: &str,
    target: &str,
    body: &impl serde::Serialize,
) -> testkit::Response {
    let bytes = serde_json::to_vec(body).expect("request json");
    exchange(app, method, target, &bytes, false).await
}

#[expect(
    clippy::panic,
    reason = "a helper outside #[test] still has to fail the test when the error body is not the gateway error"
)]
fn error_of(response: &testkit::Response) -> GatewayError {
    serde_json::from_slice(&response.body).unwrap_or_else(|error| {
        panic!(
            "error body {error}: {}",
            String::from_utf8_lossy(&response.body)
        )
    })
}

#[expect(
    clippy::panic,
    reason = "a helper outside #[test] still has to fail the test when the body is not an agent"
)]
fn agent_of(response: &testkit::Response) -> AgentPreset {
    serde_json::from_slice::<AgentResponse>(&response.body)
        .unwrap_or_else(|error| {
            panic!(
                "agent body {error}: {}",
                String::from_utf8_lossy(&response.body)
            )
        })
        .agent
}

#[expect(
    clippy::panic,
    reason = "a helper outside #[test] still has to fail the test when the body is not an agent list"
)]
fn list_of(response: &testkit::Response) -> AgentListResponse {
    serde_json::from_slice(&response.body).unwrap_or_else(|error| {
        panic!(
            "list body {error}: {}",
            String::from_utf8_lossy(&response.body)
        )
    })
}

fn named(name: &str) -> AgentPresetCreateRequest {
    AgentPresetCreateRequest {
        name: name.to_owned(),
        ..AgentPresetCreateRequest::default()
    }
}

fn assert_error(response: &testkit::Response, status: u16, message: &str) {
    assert_eq!(response.status, status);
    assert_eq!(error_of(response), GatewayError::new(message));
}

#[tokio::test]
async fn list_is_empty_until_a_preset_is_created() {
    let sandbox = Sandbox::open(None);
    let listed = exchange(&sandbox.app, "GET", "/api/agents", &[], false).await;
    assert_eq!(listed.status, 200);
    let body = list_of(&listed);
    assert_eq!(body.agents, []);
    assert_eq!(body.node, NODE);
    assert_eq!(body.backend, Some(AgentStoreBackend::File));
    assert_eq!(
        body.directory_root.as_deref(),
        Some(sandbox.path("agents").to_str().expect("utf-8 root"))
    );
    assert_eq!(
        body.shared_dir.as_deref(),
        Some(sandbox.path("shared").to_str().expect("utf-8 shared"))
    );

    let created = send_json(&sandbox.app, "POST", "/api/agents", &named("Kit")).await;
    assert_eq!(created.status, 201);
    let agent = agent_of(&created);
    assert!(generated_id(&agent.id));
    assert_eq!(agent.name, "Kit");
    assert_eq!(agent.color, "");
    assert_eq!(agent.model, "");
    assert_eq!(agent.effort, "medium");
    assert_eq!(agent.system_prompt, "");
    assert_eq!(agent.node.as_deref(), Some(NODE));
    assert_eq!(agent.shared_link, Some(true));
    assert_eq!(agent.node_base_url.as_deref(), Some(""));
    assert_eq!(agent.harness_id, None);
    assert_eq!(agent.sort_order, None);
    assert_eq!(agent.created_at, agent.updated_at);
    let directory = sandbox.path("agents").join(&agent.id);
    assert_eq!(agent.directory.as_deref(), directory.to_str());
    assert!(directory.is_dir());
    assert!(directory.starts_with(sandbox.path("agents")));
    assert!(!directory.starts_with(sibling_root(&sandbox)));
    let link = std::fs::read_link(directory.join("rivet-shared")).expect("shared link");
    assert_eq!(link, sandbox.path("shared"));

    let again = exchange(
        &sandbox.app,
        "GET",
        &format!("/api/agents/{}", agent.id),
        &[],
        false,
    )
    .await;
    assert_eq!(again.status, 200);
    assert_eq!(agent_of(&again), agent);

    let stored = std::fs::read(sandbox.path("state").join("agents.json")).expect("registry file");
    let file: AgentsFile = serde_json::from_slice(&stored).expect("registry json");
    assert_eq!(file.agents, vec![agent]);
}

#[tokio::test]
async fn defaults_and_limits_follow_the_boundary() {
    let sandbox = Sandbox::open(None);
    let unnamed = send_json(
        &sandbox.app,
        "POST",
        "/api/agents",
        &AgentPresetCreateRequest::default(),
    )
    .await;
    assert_eq!(unnamed.status, 201);
    assert_eq!(agent_of(&unnamed).name, "Unnamed Agent");
    let duplicate = exchange(&sandbox.app, "POST", "/api/agents", b"{}", false).await;
    assert_error(
        &duplicate,
        409,
        "an agent named \"Unnamed Agent\" already exists",
    );

    let mut lower = named("kit");
    lower.color = Some("#abc".to_owned());
    lower.model = Some("bad model!".to_owned());
    lower.effort = Some(String::new());
    lower.system_prompt = Some("be brief".to_owned());
    lower.shared_link = Some(false);
    lower.node = Some(NODE.to_owned());
    lower.sort_order = Some(0);
    let created = send_json(&sandbox.app, "POST", "/api/agents", &lower).await;
    assert_eq!(created.status, 201);
    let agent = agent_of(&created);
    assert_eq!(agent.name, "kit");
    assert_eq!(agent.color, "#abc");
    assert_eq!(agent.model, "bad model!");
    assert_eq!(agent.effort, "");
    assert_eq!(agent.system_prompt, "be brief");
    assert_eq!(agent.shared_link, Some(false));
    assert_eq!(agent.sort_order, Some(0));
    let kit_dir = PathBuf::from(agent.directory.expect("directory"));
    assert!(std::fs::symlink_metadata(kit_dir.join("rivet-shared")).is_err());

    let wide = "😀".repeat(128);
    let mut emoji = named(&wide);
    emoji.color = Some("#abcdef".to_owned());
    emoji.model = Some("m".repeat(128));
    emoji.effort = Some("e".repeat(64));
    emoji.node_base_url = Some("u".repeat(512));
    emoji.sort_order = Some(1_000_000);
    let accepted = send_json(&sandbox.app, "POST", "/api/agents", &emoji).await;
    assert_eq!(accepted.status, 201);
    let stored = agent_of(&accepted);
    assert_eq!(stored.name.chars().count(), 128);
    assert_eq!(stored.color, "#abcdef");
    assert_eq!(stored.model.chars().count(), 128);
    assert_eq!(stored.effort.chars().count(), 64);
    assert_eq!(
        stored
            .node_base_url
            .as_deref()
            .map(str::chars)
            .map(Iterator::count),
        Some(512)
    );

    let mut token = named("Token");
    token.effort = Some("A._[]:-9".to_owned());
    let created = send_json(&sandbox.app, "POST", "/api/agents", &token).await;
    assert_eq!(created.status, 201);
    assert_eq!(agent_of(&created).effort, "A._[]:-9");
}

#[tokio::test]
async fn rejected_field_values_do_not_create_a_preset() {
    let sandbox = Sandbox::open(Some(vec![HarnessId::GrokBuild]));
    let cases = [
        (
            r##"{"name":"Too Long","color":"#ab"}"##,
            "color must be a hex value",
        ),
        (
            r##"{"name":"Too Long","color":"#abcd"}"##,
            "color must be a hex value",
        ),
        (
            r##"{"name":"Too Long","color":"#aabbccdd"}"##,
            "color must be a hex value",
        ),
        (
            r#"{"name":"Too Long","color":"red"}"#,
            "color must be a hex value",
        ),
        (
            r#"{"name":"Too Long","model":"bad\nmodel"}"#,
            "model must not contain control characters",
        ),
        (
            r#"{"name":"Too Long","effort":"a b"}"#,
            "effort must be a 0-64 token",
        ),
        (
            r#"{"name":"Too Long","effort":"nope!"}"#,
            "effort must be a 0-64 token",
        ),
        (
            r#"{"name":"Elsewhere","node":"other-node"}"#,
            "agent must be created on its hosting node (golden-node)",
        ),
        (
            r#"{"name":"Relative","directory":"relative/agent"}"#,
            "directory must be an absolute path",
        ),
        (
            r#"{"name":"Sort","sortOrder":1000001}"#,
            "sortOrder must be an integer from 0 to 1000000",
        ),
    ];
    for (body, message) in cases {
        let response = exchange(&sandbox.app, "POST", "/api/agents", body.as_bytes(), false).await;
        assert_error(&response, 400, message);
    }
    let long_dir = format!(r#"{{"name":"LongDir","directory":"/{}"}}"#, "a".repeat(512));
    let response = exchange(
        &sandbox.app,
        "POST",
        "/api/agents",
        long_dir.as_bytes(),
        false,
    )
    .await;
    assert_error(&response, 400, "directory must be at most 512 bytes");
    let nul = exchange(
        &sandbox.app,
        "POST",
        "/api/agents",
        br#"{"name":"Nul","directory":"/tmp/agent\u0000"}"#,
        false,
    )
    .await;
    assert_error(&nul, 400, "directory must not contain NUL");
    let listed = list_of(&exchange(&sandbox.app, "GET", "/api/agents", &[], false).await);
    assert_eq!(listed.agents, []);
}

#[tokio::test]
async fn overlong_fields_do_not_create_a_preset() {
    let sandbox = Sandbox::open(None);
    let long_name = format!(r#"{{"name":"{}"}}"#, "n".repeat(129));
    let response = exchange(
        &sandbox.app,
        "POST",
        "/api/agents",
        long_name.as_bytes(),
        false,
    )
    .await;
    assert_error(&response, 400, "name must be at most 128 characters");
    let long_model = format!(r#"{{"name":"M","model":"{}"}}"#, "m".repeat(129));
    let response = exchange(
        &sandbox.app,
        "POST",
        "/api/agents",
        long_model.as_bytes(),
        false,
    )
    .await;
    assert_error(&response, 400, "model must be at most 128 characters");
    let long_effort = format!(r#"{{"name":"E","effort":"{}"}}"#, "e".repeat(65));
    let response = exchange(
        &sandbox.app,
        "POST",
        "/api/agents",
        long_effort.as_bytes(),
        false,
    )
    .await;
    assert_error(&response, 400, "effort must be a 0-64 token");
    let long_url = format!(r#"{{"name":"U","nodeBaseUrl":"{}"}}"#, "u".repeat(513));
    let response = exchange(
        &sandbox.app,
        "POST",
        "/api/agents",
        long_url.as_bytes(),
        false,
    )
    .await;
    assert_error(&response, 400, "nodeBaseUrl must be at most 512 characters");
    let emoji = format!(r#"{{"name":"{}"}}"#, "😀".repeat(129));
    let response = exchange(&sandbox.app, "POST", "/api/agents", emoji.as_bytes(), false).await;
    assert_error(&response, 400, "name must be at most 128 characters");
    let listed = list_of(&exchange(&sandbox.app, "GET", "/api/agents", &[], false).await);
    assert_eq!(listed.agents, []);
}

#[tokio::test]
async fn unknown_or_disallowed_input_does_not_create_a_preset() {
    let sandbox = Sandbox::open(Some(vec![HarnessId::GrokBuild]));
    let denied = send_json(
        &sandbox.app,
        "POST",
        "/api/agents",
        &AgentPresetCreateRequest {
            name: "Denied".to_owned(),
            harness_id: Some(HarnessId::ClaudeCode.into()),
            ..AgentPresetCreateRequest::default()
        },
    )
    .await;
    assert_eq!(denied.status, 403);
    let mut expected = GatewayError::new(
        "harness \"claude-code\" is not allowed on this node (gateway.allowed_harnesses)",
    );
    expected.code = Some("harness_not_allowed".to_owned());
    expected.harness_id = Some(HarnessId::ClaudeCode);
    assert_eq!(error_of(&denied), expected);

    let unknown = exchange(
        &sandbox.app,
        "POST",
        "/api/agents",
        br#"{"name":"Nope","harnessId":"not-a-harness"}"#,
        false,
    )
    .await;
    assert_error(&unknown, 400, "harnessId must be a known harness");
    let wrong_type = exchange(
        &sandbox.app,
        "POST",
        "/api/agents",
        br#"{"name":1,"sortOrder":1.5}"#,
        false,
    )
    .await;
    assert_eq!(wrong_type.status, 400);
    let not_object = exchange(&sandbox.app, "POST", "/api/agents", b"[]", false).await;
    assert_error(&not_object, 400, "body must be a JSON object");
    let invalid = exchange(&sandbox.app, "POST", "/api/agents", b"{", false).await;
    assert_error(&invalid, 400, "invalid JSON");
    let trailing = exchange(
        &sandbox.app,
        "POST",
        "/api/agents",
        br#"{"name":"Kit"} trailing"#,
        false,
    )
    .await;
    assert_error(&trailing, 400, "invalid JSON");
    let listed = list_of(&exchange(&sandbox.app, "GET", "/api/agents", &[], false).await);
    assert_eq!(listed.agents, []);
}

#[tokio::test]
async fn paths_allow_ordinary_names_and_refuse_escapes() {
    let sandbox = Sandbox::open(None);
    let ordinary = sandbox.path("work/my agent $(id) > out");
    let mut body = named("Shell-looking name > $(id)");
    body.directory = Some(ordinary.to_str().expect("utf-8").to_owned());
    body.system_prompt = Some("quote \"this\" and $HOME".to_owned());
    let created = send_json(&sandbox.app, "POST", "/api/agents", &body).await;
    assert_eq!(
        created.status,
        201,
        "{}",
        String::from_utf8_lossy(&created.body)
    );
    let agent = agent_of(&created);
    assert_eq!(agent.name, "Shell-looking name > $(id)");
    assert_eq!(agent.system_prompt, "quote \"this\" and $HOME");
    assert!(ordinary.is_dir());
    assert_eq!(
        std::fs::read_link(ordinary.join("rivet-shared")).expect("link"),
        sandbox.path("shared")
    );

    let leaf = format!("outside-{}", std::process::id());
    let escape = sandbox.path(&format!("work/../../{leaf}"));
    let mut body = named("Escape");
    body.directory = Some(escape.to_str().expect("utf-8").to_owned());
    let refused = send_json(&sandbox.app, "POST", "/api/agents", &body).await;
    assert_error(&refused, 400, "directory escapes its requested path");
    let landed = sandbox
        .root
        .path()
        .parent()
        .expect("temp parent")
        .join(leaf);
    assert!(!landed.exists());

    let secret = sandbox.path("secret");
    let through = sandbox.path("linkdir");
    std::fs::create_dir(&secret).expect("secret");
    std::os::unix::fs::symlink(&secret, &through).expect("symlink");
    let mut body = named("Linked");
    body.directory = Some(through.join("agent").to_str().expect("utf-8").to_owned());
    let refused = send_json(&sandbox.app, "POST", "/api/agents", &body).await;
    assert_error(&refused, 400, "directory escapes its requested path");
    assert!(!secret.join("agent").exists());

    let sibling = sibling_root(&sandbox).join("p-smuggled");
    let mut body = named("Sibling");
    body.directory = Some(sibling.to_str().expect("utf-8").to_owned());
    let created = send_json(&sandbox.app, "POST", "/api/agents", &body).await;
    assert_eq!(created.status, 201);
    assert!(sibling.is_dir());
    assert!(
        !agent_of(&created)
            .directory
            .as_deref()
            .is_some_and(|path| Path::new(path).starts_with(sandbox.path("agents")))
    );
}

#[tokio::test]
async fn names_are_trimmed_and_case_insensitive() {
    let sandbox = Sandbox::open(None);
    let created = send_json(&sandbox.app, "POST", "/api/agents", &named(" Kit ")).await;
    assert_eq!(created.status, 201);
    let kit = agent_of(&created);
    assert_eq!(kit.name, "Kit");
    let duplicate = send_json(&sandbox.app, "POST", "/api/agents", &named("kit")).await;
    assert_error(&duplicate, 409, "an agent named \"kit\" already exists");
    let blank = send_json(&sandbox.app, "POST", "/api/agents", &named("   ")).await;
    assert_eq!(blank.status, 201);
    assert_eq!(agent_of(&blank).name, "Unnamed Agent");
    let other = send_json(&sandbox.app, "POST", "/api/agents", &named("Other")).await;
    assert_eq!(other.status, 201);
    let id = agent_of(&other).id;
    let spaces = exchange(
        &sandbox.app,
        "PATCH",
        &format!("/api/agents/{id}"),
        br#"{"name":"  "}"#,
        false,
    )
    .await;
    assert_error(&spaces, 400, "name must not be empty");
    let clash = exchange(
        &sandbox.app,
        "PATCH",
        &format!("/api/agents/{id}"),
        br#"{"name":"KIT"}"#,
        false,
    )
    .await;
    assert_error(&clash, 409, "an agent named \"KIT\" already exists");
}

#[tokio::test]
async fn shared_link_patch_refuses_a_swapped_directory_symlink() {
    let sandbox = Sandbox::open(None);
    let created = send_json(&sandbox.app, "POST", "/api/agents", &named("Linked")).await;
    assert_eq!(created.status, 201);
    let agent = agent_of(&created);
    let directory = PathBuf::from(agent.directory.expect("directory"));
    std::fs::remove_file(directory.join("rivet-shared")).expect("link");
    std::fs::remove_dir(&directory).expect("directory");
    let outside = sandbox.path("outside");
    std::fs::create_dir(&outside).expect("outside");
    let planted = outside.join("rivet-shared");
    std::os::unix::fs::symlink(sandbox.path("shared"), &planted).expect("plant");
    std::os::unix::fs::symlink(&outside, &directory).expect("swap");
    let refused = exchange(
        &sandbox.app,
        "PATCH",
        &format!("/api/agents/{}", agent.id),
        br#"{"sharedLink":false}"#,
        false,
    )
    .await;
    assert_error(&refused, 400, "directory escapes its requested path");
    assert_eq!(
        std::fs::read_link(&planted).expect("planted link"),
        sandbox.path("shared")
    );
    let kept = exchange(
        &sandbox.app,
        "GET",
        &format!("/api/agents/{}", agent.id),
        &[],
        false,
    )
    .await;
    assert_eq!(agent_of(&kept).shared_link, Some(true));

    let mut quiet = named("Quiet");
    quiet.shared_link = Some(false);
    let created = send_json(&sandbox.app, "POST", "/api/agents", &quiet).await;
    assert_eq!(created.status, 201);
    let agent = agent_of(&created);
    let directory = PathBuf::from(agent.directory.expect("directory"));
    std::fs::remove_dir(&directory).expect("quiet directory");
    let clean = sandbox.path("clean");
    std::fs::create_dir(&clean).expect("clean");
    std::os::unix::fs::symlink(&clean, &directory).expect("swap clean");
    let refused = exchange(
        &sandbox.app,
        "PATCH",
        &format!("/api/agents/{}", agent.id),
        br#"{"sharedLink":true}"#,
        false,
    )
    .await;
    assert_error(&refused, 400, "directory escapes its requested path");
    assert!(!clean.join("rivet-shared").exists());
}

#[tokio::test]
async fn create_moves_a_catalog_model_onto_the_harness() {
    let sandbox = Sandbox::open(None);
    let mut body = named("Claude");
    body.model = Some("claude".to_owned());
    let created = send_json(&sandbox.app, "POST", "/api/agents", &body).await;
    assert_eq!(created.status, 201);
    let agent = agent_of(&created);
    assert_eq!(agent.harness_id, Some(HarnessId::ClaudeCode));
    assert_eq!(agent.model, "");

    let mut body = named("Kimi");
    body.model = Some("kimi-code".to_owned());
    let created = send_json(&sandbox.app, "POST", "/api/agents", &body).await;
    assert_eq!(created.status, 201);
    let agent = agent_of(&created);
    assert_eq!(agent.harness_id, Some(HarnessId::KimiCode));
    assert_eq!(agent.model, "");

    let mut body = named("Bad");
    body.model = Some("bad model!".to_owned());
    body.harness_id = Some(HarnessId::GrokBuild.into());
    let created = send_json(&sandbox.app, "POST", "/api/agents", &body).await;
    assert_eq!(created.status, 201);
    let agent = agent_of(&created);
    assert_eq!(agent.model, "bad model!");
    assert_eq!(agent.harness_id, Some(HarnessId::GrokBuild));

    let limited = Sandbox::open(Some(vec![HarnessId::GrokBuild]));
    let mut body = named("Moved");
    body.model = Some("claude".to_owned());
    let denied = send_json(&limited.app, "POST", "/api/agents", &body).await;
    assert_eq!(denied.status, 403);
    let mut expected = GatewayError::new(
        "harness \"claude-code\" is not allowed on this node (gateway.allowed_harnesses)",
    );
    expected.code = Some("harness_not_allowed".to_owned());
    expected.harness_id = Some(HarnessId::ClaudeCode);
    assert_eq!(error_of(&denied), expected);
}

#[tokio::test]
async fn shared_link_tracks_the_flag_without_taking_a_foreign_link() {
    let sandbox = Sandbox::open(None);
    let directory = sandbox.path("owned");
    std::fs::create_dir(&directory).expect("directory");
    let foreign = directory.join("rivet-shared");
    std::os::unix::fs::symlink(sandbox.path("secret"), &foreign).expect("foreign link");
    let mut body = named("Keep");
    body.directory = Some(directory.to_str().expect("utf-8").to_owned());
    body.shared_link = Some(false);
    let created = send_json(&sandbox.app, "POST", "/api/agents", &body).await;
    assert_eq!(created.status, 201);
    assert_eq!(
        std::fs::read_link(&foreign).expect("kept"),
        sandbox.path("secret")
    );

    let mut body = named("Replace");
    body.directory = Some(directory.to_str().expect("utf-8").to_owned());
    body.shared_link = Some(true);
    let refused = send_json(&sandbox.app, "POST", "/api/agents", &body).await;
    assert_eq!(refused.status, 500);
    assert_eq!(
        error_of(&refused).error,
        "could not create agent directory: rivet-shared points elsewhere"
    );
    assert_eq!(
        std::fs::read_link(&foreign).expect("untouched"),
        sandbox.path("secret")
    );
    let listed = list_of(&exchange(&sandbox.app, "GET", "/api/agents", &[], false).await);
    assert_eq!(listed.agents.len(), 1);
}

#[tokio::test]
async fn patch_and_delete_follow_the_boundary() {
    let sandbox = Sandbox::open(Some(vec![HarnessId::ClaudeCode]));
    let created = send_json(
        &sandbox.app,
        "POST",
        "/api/agents",
        &AgentPresetCreateRequest {
            name: "Kit".to_owned(),
            color: Some("#fff".to_owned()),
            harness_id: Some(HarnessId::ClaudeCode.into()),
            sort_order: Some(2),
            ..AgentPresetCreateRequest::default()
        },
    )
    .await;
    let original = agent_of(&created);
    let id = original.id.clone();
    let same = send_json(
        &sandbox.app,
        "PATCH",
        &format!("/api/agents/{id}"),
        &AgentPresetUpdate::default(),
    )
    .await;
    assert_eq!(same.status, 200);
    assert_eq!(agent_of(&same), original);

    let renamed = send_json(
        &sandbox.app,
        "PATCH",
        &format!("/api/agents/{id}"),
        &AgentPresetUpdate {
            name: Some("Kit Two".to_owned()),
            color: Some("#abc".to_owned()),
            ..AgentPresetUpdate::default()
        },
    )
    .await;
    assert_eq!(renamed.status, 200);
    let updated = agent_of(&renamed);
    assert_eq!(updated.name, "Kit Two");
    assert_eq!(updated.color, "#abc");
    assert_eq!(updated.created_at, original.created_at);
    assert!(updated.updated_at >= original.updated_at);
    assert_eq!(updated.harness_id, Some(HarnessId::ClaudeCode));
}

#[tokio::test]
async fn patch_clears_nullable_fields_and_rejects_bad_edits() {
    let sandbox = Sandbox::open(Some(vec![HarnessId::ClaudeCode]));
    let created = send_json(
        &sandbox.app,
        "POST",
        "/api/agents",
        &AgentPresetCreateRequest {
            name: "Kit".to_owned(),
            harness_id: Some(HarnessId::ClaudeCode.into()),
            sort_order: Some(2),
            ..AgentPresetCreateRequest::default()
        },
    )
    .await;
    let id = agent_of(&created).id;
    let cleared = send_json(
        &sandbox.app,
        "PATCH",
        &format!("/api/agents/{id}"),
        &AgentPresetUpdate {
            harness_id: Some(None),
            sort_order: Some(None),
            ..AgentPresetUpdate::default()
        },
    )
    .await;
    assert_eq!(cleared.status, 200);
    assert_eq!(agent_of(&cleared).harness_id, None);
    assert_eq!(agent_of(&cleared).sort_order, None);

    let colored = exchange(
        &sandbox.app,
        "PATCH",
        &format!("/api/agents/{id}"),
        br##"{"color":"#fff"}"##,
        false,
    )
    .await;
    assert_eq!(colored.status, 200);
    let blank = exchange(
        &sandbox.app,
        "PATCH",
        &format!("/api/agents/{id}"),
        br#"{"color":""}"#,
        false,
    )
    .await;
    assert_eq!(blank.status, 200);
    assert_eq!(agent_of(&blank).color, "");

    let immutable = exchange(
        &sandbox.app,
        "PATCH",
        &format!("/api/agents/{id}"),
        br#"{"node":"other-node"}"#,
        false,
    )
    .await;
    assert_error(&immutable, 400, "node is immutable; recreate the agent");
    let empty_name = exchange(
        &sandbox.app,
        "PATCH",
        &format!("/api/agents/{id}"),
        br#"{"name":""}"#,
        false,
    )
    .await;
    assert_error(&empty_name, 400, "name must not be empty");
}

#[tokio::test]
async fn delete_removes_the_preset_and_leaves_its_directory() {
    let sandbox = Sandbox::open(None);
    let created = send_json(&sandbox.app, "POST", "/api/agents", &named("Kit")).await;
    let original = agent_of(&created);
    let id = original.id.clone();
    let removed = exchange(
        &sandbox.app,
        "DELETE",
        &format!("/api/agents/{id}"),
        &[],
        false,
    )
    .await;
    assert_eq!(removed.status, 200);
    assert_eq!(
        serde_json::from_slice::<OkReply>(&removed.body).expect("ok"),
        OkReply { ok: true }
    );
    let missing = exchange(
        &sandbox.app,
        "GET",
        &format!("/api/agents/{id}"),
        &[],
        false,
    )
    .await;
    assert_error(&missing, 404, "agent not found");
    let again = exchange(
        &sandbox.app,
        "DELETE",
        &format!("/api/agents/{id}"),
        &[],
        false,
    )
    .await;
    assert_error(&again, 404, "agent not found");
    assert!(Path::new(original.directory.as_deref().expect("directory")).is_dir());
}

#[tokio::test]
async fn recorded_presets_round_trip_and_refuse_foreign_edits() {
    let sandbox = Sandbox::open(None);
    let text = recorded_agents_text();
    let recorded: AgentsFile = serde_json::from_str(&text).expect("recorded agents");
    std::fs::create_dir_all(sandbox.path("state")).expect("state dir");
    std::fs::write(sandbox.path("state").join("agents.json"), &text).expect("seed");
    let listed = list_of(
        &exchange(
            &sandbox.app,
            "GET",
            "/api/agents?node=other-node",
            &[],
            false,
        )
        .await,
    );
    assert_eq!(listed.agents.len(), 1);
    let remote = listed.agents.first().expect("filtered preset");
    assert_eq!(remote.id, "p-remote");
    assert_eq!(remote.name, "Remote Agent");
    let local = list_of(
        &exchange(
            &sandbox.app,
            "GET",
            "/api/agents?node=golden-node",
            &[],
            false,
        )
        .await,
    );
    assert!(
        local
            .agents
            .iter()
            .all(|agent| agent.node.as_deref() == Some(NODE))
    );
    assert!(local.agents.iter().all(|agent| agent.id != "p-remote"));
    let all = list_of(&exchange(&sandbox.app, "GET", "/api/agents", &[], false).await);
    assert_recorded_registry_order(&all.agents, &recorded.agents);

    let remote = agent_of(&exchange(&sandbox.app, "GET", "/api/agents/p-remote", &[], false).await);
    assert_eq!(remote.node.as_deref(), Some("other-node"));
    let patched = exchange(
        &sandbox.app,
        "PATCH",
        "/api/agents/p-remote",
        br#"{"name":"Taken"}"#,
        false,
    )
    .await;
    assert_eq!(patched.status, 409);
    let mut hosted = GatewayError::new("agent \"Remote Agent\" is hosted on other-node");
    hosted.node = Some("other-node".to_owned());
    assert_eq!(error_of(&patched), hosted);
    let deleted = exchange(&sandbox.app, "DELETE", "/api/agents/p-remote", &[], false).await;
    assert_eq!(error_of(&deleted), hosted);
    assert_eq!(deleted.status, 409);

    let relative = exchange(
        &sandbox.app,
        "PATCH",
        "/api/agents/p-reldir",
        br#"{"sharedLink":true}"#,
        false,
    )
    .await;
    assert_error(&relative, 400, "directory must be an absolute path");
    let bad_id = exchange(&sandbox.app, "GET", "/api/agents/bad.id", &[], false).await;
    assert_error(&bad_id, 404, "agent not found");
    let method = exchange(&sandbox.app, "PUT", "/api/agents/p-acme", &[], false).await;
    assert_error(&method, 405, "method not allowed");
}

#[tokio::test]
async fn registry_file_failures_stay_closed() {
    let sandbox = Sandbox::open(None);
    std::fs::create_dir_all(sandbox.path("state")).expect("state");
    std::fs::write(sandbox.path("state").join("agents.json"), b"{").expect("corrupt");
    let listed = exchange(&sandbox.app, "GET", "/api/agents", &[], false).await;
    assert_error(&listed, 503, "agent registry unavailable");

    let sandbox = Sandbox::open(None);
    std::fs::create_dir_all(sandbox.path("state")).expect("state");
    let target = sandbox.path("outside.json");
    std::fs::write(&target, b"secret").expect("target");
    std::os::unix::fs::symlink(&target, sandbox.path("state").join("agents.json")).expect("link");
    let created = send_json(&sandbox.app, "POST", "/api/agents", &named("Nope")).await;
    assert_error(&created, 503, "agent registry unavailable");
    assert_eq!(std::fs::read(&target).expect("target bytes"), b"secret");
}

#[tokio::test]
async fn auth_still_wraps_the_routes() {
    let sandbox = Sandbox::open(None);
    let remote = exchange(&sandbox.app, "GET", "/api/agents", &[], true).await;
    assert_error(&remote, 401, "unauthorized");
    let options = exchange(&sandbox.app, "OPTIONS", "/api/agents", &[], false).await;
    assert_eq!(options.status, 204);
    let huge = vec![b'a'; (256 * 1024) + 8];
    let limited = exchange(&sandbox.app, "POST", "/api/agents", &huge, false).await;
    assert_error(&limited, 413, "body too large");
}

#[tokio::test]
async fn a_reopened_registry_reads_the_file_it_wrote() {
    let root = tempfile::tempdir().expect("root");
    let id = {
        let gateway = gateway_at(root.path(), None);
        let app = gateway.router(false);
        let created = send_json(&app, "POST", "/api/agents", &named("Durable")).await;
        assert_eq!(created.status, 201);
        let agent = agent_of(&created);
        drop(app);
        drop(gateway);
        agent.id
    };
    let gateway = gateway_at(root.path(), None);
    let app = gateway.router(false);
    let loaded = exchange(&app, "GET", &format!("/api/agents/{id}"), &[], false).await;
    assert_eq!(loaded.status, 200);
    assert_eq!(agent_of(&loaded).name, "Durable");
}

#[tokio::test]
async fn concurrent_creates_keep_distinct_presets() {
    let sandbox = Sandbox::open(None);
    let app = sandbox.app.clone();
    let first = async { send_json(&app, "POST", "/api/agents", &named("One")).await };
    let second = async { send_json(&app, "POST", "/api/agents", &named("Two")).await };
    let (left, right) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(first, second)
    })
    .await
    .expect("concurrent creates timed out");
    assert_eq!(left.status, 201);
    assert_eq!(right.status, 201);
    assert_ne!(agent_of(&left).id, agent_of(&right).id);
    let listed = list_of(&exchange(&sandbox.app, "GET", "/api/agents", &[], false).await);
    assert_eq!(listed.agents.len(), 2);
}

#[test]
fn settings_use_the_hub_layout_and_known_harnesses() {
    let root = tempfile::tempdir().expect("home");
    let shared = root.path().join("shared-root");
    let env = config::Environment::from_vars([
        ("HOME", root.path().to_str().expect("utf-8 home")),
        (
            "RIVETHUB_SHARED_DIR",
            shared.to_str().expect("utf-8 shared"),
        ),
    ]);
    let config: config::Config = serde_yaml_ng::from_str(
        "gateway:\n  allowed_harnesses:\n    - claude-code\n    - not-a-real-harness\n",
    )
    .expect("config");
    let settings = Settings::from_config_in(&config, &env, "gold-host".to_owned(), NODE.to_owned())
        .expect("settings");
    let state = root.path().join(".rivethub");
    assert_eq!(settings.state_dir, state);
    assert_eq!(settings.directory_root, state.join("agents"));
    assert_eq!(settings.shared_dir, shared);
    assert_eq!(
        settings.allowed_harnesses,
        Some(vec![HarnessId::ClaudeCode])
    );
}

#[test]
fn settings_resolve_a_symlinked_home() {
    let root = tempfile::tempdir().expect("home");
    let links = tempfile::tempdir().expect("links");
    let home = links.path().join("home");
    std::os::unix::fs::symlink(root.path(), &home).expect("home link");
    let env = config::Environment::from_vars([
        ("HOME", home.to_str().expect("utf-8 home")),
        (
            "RIVETHUB_SHARED_DIR",
            root.path().to_str().expect("utf-8 shared"),
        ),
    ]);
    let config: config::Config = serde_yaml_ng::from_str("").expect("config");
    let settings = Settings::from_config_in(&config, &env, "gold-host".to_owned(), NODE.to_owned())
        .expect("settings");
    let real = std::fs::canonicalize(root.path()).expect("real home");
    assert_eq!(settings.state_dir, real.join(".rivethub"));
    assert_eq!(
        settings.directory_root,
        real.join(".rivethub").join("agents")
    );
    assert_eq!(settings.shared_dir, real);
}

const CLAIMED_AGENT_CASES: &[&str] = &[];

const SKIPPED_AGENT_CASES: &[(&str, &str)] = &[];

#[test]
fn recorded_agent_cases_are_replayed_or_listed() {
    let recordings = [
        testkit::recording("gateway/http-plain.json"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../wire/tests/golden/gateway/http-tls.json"),
    ];
    let mut total = 0usize;
    let mut agent_cases = Vec::new();
    for recording in &recordings {
        let cases = testkit::http::cases(recording)
            .unwrap_or_else(|error| panic!("{}: {error}", recording.display()));
        assert!(!cases.is_empty(), "{}", recording.display());
        total += cases.len();
        for case in cases {
            let Some(head) = testkit::http::parse_head(&case.request.head) else {
                assert!(
                    !case.request.head.contains("/api/agents"),
                    "{} has an unreadable agent request",
                    case.id
                );
                continue;
            };
            let path = head
                .target
                .split_once('?')
                .map_or(head.target.as_str(), |(path, _)| path);
            if path == "/api/agents" || path.starts_with("/api/agents/") {
                agent_cases.push(case.id);
            }
        }
    }
    assert!(total > 0);
    let mut unaccounted = Vec::new();
    for id in &agent_cases {
        let claimed = CLAIMED_AGENT_CASES.contains(&id.as_str());
        let skipped = SKIPPED_AGENT_CASES
            .iter()
            .any(|(name, reason)| name == id && !reason.is_empty());
        if !claimed && !skipped {
            unaccounted.push(id.clone());
        }
    }
    assert!(
        unaccounted.is_empty(),
        "agent cases neither replayed nor skipped: {unaccounted:?}"
    );
    for name in CLAIMED_AGENT_CASES {
        assert!(
            agent_cases.iter().any(|id| id == name),
            "claimed case is not in the recordings: {name}"
        );
    }
    for (name, reason) in SKIPPED_AGENT_CASES {
        assert_ne!(*reason, "");
        assert!(
            agent_cases.iter().any(|id| id == name),
            "skipped case is not in the recordings: {name}"
        );
    }
}

#[expect(
    clippy::expect_used,
    reason = "a helper outside #[test] still has to fail the test when a preset from the contract file is missing"
)]
fn assert_recorded_registry_order(listed: &[AgentPreset], recorded: &[AgentPreset]) {
    let mut ordered = recorded.to_vec();
    ordered.sort_by(|left, right| {
        let rank = match (left.sort_order, right.sort_order) {
            (Some(first), Some(second)) => first.cmp(&second),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        rank.then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    let ids: Vec<_> = listed.iter().map(|agent| agent.id.as_str()).collect();
    let expected: Vec<_> = ordered.iter().map(|agent| agent.id.as_str()).collect();
    assert_eq!(ids, expected);
    assert_eq!(ids.first().copied(), Some("p-acme"));
    assert_eq!(ids.get(1).copied(), Some("p-acme-two"));
    assert_eq!(ids.get(2).copied(), Some("p-badmodel"));
    for agent in recorded {
        let found = listed
            .iter()
            .find(|item| item.id == agent.id)
            .expect("recorded preset");
        assert_eq!(found, agent);
    }
}

fn generated_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("p-") else {
        return false;
    };
    rest.len() == 12 && rest.chars().all(|character| character.is_ascii_hexdigit())
}

fn sibling_root(sandbox: &Sandbox) -> PathBuf {
    let root = sandbox.path("agents");
    PathBuf::from(format!("{}-evil", root.display()))
}

#[expect(
    clippy::expect_used,
    reason = "a helper outside #[test] still has to fail the test when the contract agents file cannot be read"
)]
fn recorded_agents_text() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../wire/tests/golden/term/http.json");
    let text = std::fs::read_to_string(&path).expect("term golden");
    let document: TermDocument = serde_json::from_str(&text).expect("term golden json");
    document
        .runs
        .get("term-main")
        .and_then(|run| run.state_after.as_ref())
        .and_then(|state| state.agents_json.as_ref())
        .expect("recorded agents.json")
        .text
        .clone()
}

#[derive(serde::Deserialize)]
struct TermDocument {
    runs: std::collections::BTreeMap<String, TermRun>,
}

#[derive(serde::Deserialize)]
struct TermRun {
    #[serde(rename = "stateAfter", default)]
    state_after: Option<TermState>,
}

#[derive(serde::Deserialize)]
struct TermState {
    #[serde(rename = "agents.json", default)]
    agents_json: Option<TermFile>,
}

#[derive(serde::Deserialize)]
struct TermFile {
    text: String,
}
