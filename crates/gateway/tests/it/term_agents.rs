#![cfg(test)]
use crate::session_support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gateway::tenancy::Tenancy;
use gateway::term::{LaunchCommand, TerminalOptions, TerminalService};
use session_support::{Call, Mux, http, user};
use wire::gateway::GatewayError;
use wire::gateway::agents::AgentPreset;
use wire::gateway::term::{TermSpawnRequest, TermSpawnResponse};
use wire::harness::HarnessId;
use wire::json::Extra;

const NODE: &str = "golden-node";

struct Lab {
    gateway: Arc<gateway::Gateway>,
    mux: Arc<Mux>,
    harness: Arc<session_support::harness::Harness>,
    root: tempfile::TempDir,
}

impl Lab {
    async fn open(agents: &[AgentPreset]) -> Self {
        let root = tempfile::tempdir().expect("lab directory");
        std::fs::create_dir_all(root.path().join("state")).expect("state directory");
        std::fs::create_dir_all(root.path().join("shared")).expect("shared directory");
        let file = serde_json::to_vec(&AgentsFile { agents }).expect("agents file");
        std::fs::write(root.path().join("state/agents.json"), file).expect("write agents");
        Self::serve(root).await
    }

    async fn serve(root: tempfile::TempDir) -> Self {
        let registry = Arc::new(gateway::sessions::SessionRegistry::new(
            Duration::from_secs(1),
        ));
        let mux = Arc::new(Mux::default());
        let harness = Arc::new(session_support::harness::Harness::default());
        let commands = [
            ("shell", None),
            ("claude", Some(HarnessId::ClaudeCode)),
            ("grok", Some(HarnessId::GrokBuild)),
        ]
        .into_iter()
        .map(|(id, harness)| LaunchCommand {
            id: id.into(),
            label: id.into(),
            argv: vec![id.into()],
            cwd: root.path().to_path_buf(),
            harness,
            session: harness.is_some(),
            env: BTreeMap::new(),
        })
        .collect();
        let service = TerminalService::new(
            mux.clone(),
            registry.clone(),
            TerminalOptions {
                roots: vec![root.path().to_path_buf()],
                default: "shell".into(),
                commands,
                max_ptys: 8,
            },
        )
        .await
        .expect("terminal service")
        .with_harness(harness.clone());
        let gateway = Arc::new(
            gateway::Gateway::new(
                Arc::new(testkit::FakeMemoryStore::default()),
                gateway::Settings {
                    hostname: "gold-host".into(),
                    node: NODE.into(),
                    allowed_hosts: vec![],
                    allowed_origins: vec![],
                    files_root: None,
                    static_dir: None,
                    root_redirect: None,
                    state_dir: root.path().join("state"),
                    directory_root: root.path().join("agents"),
                    shared_dir: root.path().join("shared"),
                    allowed_harnesses: None,
                    devices_enabled: false,
                    probe_timeout: Duration::from_millis(200),
                    mesh_ca: None,
                },
            )
            .with_tenancy(Tenancy::local(user("owner")))
            .with_sessions(registry)
            .with_terminal(Arc::new(service)),
        );
        Self {
            gateway,
            mux,
            harness,
            root,
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    async fn send(&self, path: &str, body: &TermSpawnRequest) -> (u16, Vec<u8>) {
        http(
            &self.gateway,
            "POST",
            path,
            serde_json::to_vec(body).expect("body"),
            None,
        )
        .await
    }

    async fn refuse(&self, path: &str, body: &TermSpawnRequest) -> (u16, String, Option<String>) {
        let (status, bytes) = self.send(path, body).await;
        let reply: GatewayError = serde_json::from_slice(&bytes).expect("error reply");
        (status, reply.error, reply.node)
    }

    async fn start(&self, path: &str, body: &TermSpawnRequest) -> (u16, Option<TermSpawnResponse>) {
        let (status, bytes) = self.send(path, body).await;
        (status, serde_json::from_slice(&bytes).ok())
    }

    fn spawns(&self) -> Vec<(Vec<String>, PathBuf)> {
        self.mux
            .calls
            .borrow()
            .iter()
            .filter_map(|call| match call {
                Call::Spawn { argv, cwd, .. } => Some((argv.clone(), cwd.clone())),
                Call::Input(_) | Call::Resize(_) | Call::Kill => None,
            })
            .collect()
    }
}

#[derive(serde::Serialize)]
struct AgentsFile<'a> {
    agents: &'a [AgentPreset],
}

fn preset(id: &str, edit: impl FnOnce(&mut AgentPreset)) -> AgentPreset {
    let at = DateTime::<Utc>::from_timestamp_millis(1_760_000_000_000).expect("fixed instant");
    let mut preset = AgentPreset {
        id: id.to_owned(),
        name: format!("Agent {id}"),
        color: String::new(),
        harness_id: Some(HarnessId::ClaudeCode),
        model: String::new(),
        effort: "medium".to_owned(),
        system_prompt: String::new(),
        node: Some(NODE.to_owned()),
        directory: None,
        shared_link: Some(true),
        node_base_url: Some(String::new()),
        sort_order: None,
        created_at: at,
        updated_at: at,
        extra: Extra::default(),
    };
    edit(&mut preset);
    preset
}

fn request(agent: &str) -> TermSpawnRequest {
    TermSpawnRequest {
        command: None,
        model: None,
        effort: None,
        session: None,
        resume: None,
        agent_id: Some(agent.to_owned()),
        force: None,
        cols: None,
        rows: None,
        topic_id: None,
    }
}

fn refused(status: u16, message: &str) -> (u16, String, Option<String>) {
    (status, message.to_owned(), None)
}

fn directory(root: &Path, relative: &str) -> String {
    root.join(relative).to_str().expect("utf-8 path").to_owned()
}

#[tokio::test]
async fn agent_id_must_be_a_token() {
    let lab = Lab::open(&[]).await;
    for id in ["", "has space", "dot.ted", &"a".repeat(65)] {
        let reply = lab.refuse("/api/terminal", &request(id)).await;
        assert_eq!(reply, refused(400, "agentId must be a 1-64 token"), "{id}");
    }
    assert_eq!(lab.spawns(), Vec::new());
}

#[tokio::test]
async fn store_failure_and_missing_preset() {
    let root = tempfile::tempdir().expect("lab directory");
    std::fs::create_dir_all(root.path().join("state")).expect("state directory");
    std::fs::write(root.path().join("state/agents.json"), b"not json").expect("write");
    let broken = Lab::serve(root).await;
    assert_eq!(
        broken.refuse("/api/terminal", &request("p-one")).await,
        refused(503, "agent registry unavailable")
    );
    let lab = Lab::open(&[]).await;
    assert_eq!(
        lab.refuse("/term", &request("p-one")).await,
        refused(404, "agent not found")
    );
}

#[tokio::test]
async fn preset_hosted_elsewhere_conflicts() {
    let lab = Lab::open(&[preset("p-far", |p| {
        p.node = Some("other-node".to_owned());
        p.directory = Some("/srv/far".to_owned());
    })])
    .await;
    assert_eq!(
        lab.refuse("/api/terminal", &request("p-far")).await,
        (
            409,
            "agent \"Agent p-far\" is hosted on other-node".to_owned(),
            Some("other-node".to_owned())
        )
    );
}

#[tokio::test]
async fn preset_without_directory_conflicts() {
    let lab = Lab::open(&[
        preset("p-none", |_| ()),
        preset("p-blank", |p| p.directory = Some("  //  ".to_owned())),
    ])
    .await;
    for id in ["p-none", "p-blank"] {
        assert_eq!(
            lab.refuse("/api/terminal", &request(id)).await,
            refused(409, &format!("agent \"Agent {id}\" has no directory"))
        );
    }
}

#[tokio::test]
async fn directory_failure_is_a_server_error() {
    let root = tempfile::tempdir().expect("lab directory");
    let occupied = directory(root.path(), "occupied");
    std::fs::write(&occupied, b"file").expect("occupy path");
    let lab = Lab::open(&[preset("p-file", |p| p.directory = Some(occupied))]).await;
    assert_eq!(
        lab.refuse("/api/terminal", &request("p-file")).await,
        refused(
            500,
            "could not create agent directory: agent directory path exists and is not a directory"
        )
    );
}

#[tokio::test]
async fn preset_needs_a_harness_or_a_command() {
    let root = tempfile::tempdir().expect("lab directory");
    let home = directory(root.path(), "plain");
    let lab = Lab::open(&[preset("p-plain", |p| {
        p.harness_id = None;
        p.directory = Some(home);
    })])
    .await;
    assert_eq!(
        lab.refuse("/api/terminal", &request("p-plain")).await,
        refused(400, "agent has no harness and no command was given")
    );
    let mut shell = request("p-plain");
    shell.command = Some("shell".to_owned());
    shell.session = Some("plain".to_owned());
    let (status, _) = lab.start("/api/terminal", &shell).await;
    assert_eq!(status, 201);
    assert_eq!(
        lab.spawns(),
        vec![(
            vec![
                "shell".to_owned(),
                "--effort".to_owned(),
                "medium".to_owned()
            ],
            root.path().join("plain")
        )]
    );
}

#[tokio::test]
async fn stored_model_and_effort_are_validated() {
    let root = tempfile::tempdir().expect("lab directory");
    let home = directory(root.path(), "home");
    let lab = Lab::open(&[
        preset("p-model", |p| {
            p.model = "bad model".to_owned();
            p.directory = Some(home.clone());
        }),
        preset("p-effort", |p| {
            p.effort = "a..b".to_owned();
            p.directory = Some(home.clone());
        }),
    ])
    .await;
    assert_eq!(
        lab.refuse("/api/terminal", &request("p-model")).await,
        refused(400, "agent \"Agent p-model\" model must be a 1-64 token")
    );
    assert_eq!(
        lab.refuse("/api/terminal", &request("p-effort")).await,
        refused(400, "agent \"Agent p-effort\" effort must be a 1-64 token")
    );
    assert_eq!(
        lab.refuse("/api/terminal", &{
            let mut bad = request("p-model");
            bad.model = Some("bad model".to_owned());
            bad
        })
        .await,
        refused(400, "model must be a 1-64 token")
    );
    for junk in ["/opus", "ds41//flash", "ds41/"] {
        let mut bad = request("p-model");
        bad.model = Some(junk.to_owned());
        assert_eq!(
            lab.refuse("/api/terminal", &bad).await,
            refused(400, "model must be a 1-64 token"),
            "{junk}"
        );
    }
    let mut fixed = request("p-model");
    fixed.model = Some("local-27b/qwen-27b".to_owned());
    fixed.session = Some("fixed".to_owned());
    let (status, _) = lab.start("/api/terminal", &fixed).await;
    assert_eq!(status, 201);
    let spawns = lab.spawns();
    assert!(
        spawns
            .iter()
            .all(|(argv, _)| !argv.contains(&"bad model".to_owned()))
    );
    assert!(
        spawns
            .iter()
            .any(|(argv, _)| argv.contains(&"local-27b/qwen-27b".to_owned()))
    );
}

#[tokio::test]
async fn spawn_takes_command_cwd_and_flags_from_the_preset() {
    let root = tempfile::tempdir().expect("lab directory");
    let home = format!("{}/", directory(root.path(), "agents/p-one"));
    let lab = Lab::open(&[preset("p-one", |p| {
        p.model = "opus".to_owned();
        p.effort = "high".to_owned();
        p.directory = Some(home);
    })])
    .await;
    let mut run = request("p-one");
    run.session = Some("preset-run".to_owned());
    let (status, body) = lab.start("/api/terminal", &run).await;
    assert_eq!(status, 201);
    assert_eq!(body.map(|reply| reply.command), Some("claude".to_owned()));
    let normalized = root.path().join("agents/p-one");
    assert!(normalized.is_dir());
    assert_eq!(
        std::fs::read_link(normalized.join("rivet-shared")).expect("shared link"),
        lab.path("shared")
    );
    assert_eq!(
        lab.spawns(),
        vec![(
            vec![
                "claude".to_owned(),
                "--model".to_owned(),
                "opus".to_owned(),
                "--effort".to_owned(),
                "high".to_owned()
            ],
            normalized.clone()
        )]
    );
    let mut explicit = request("p-one");
    explicit.session = Some("explicit-run".to_owned());
    explicit.command = Some("grok".to_owned());
    explicit.model = Some("fast".to_owned());
    explicit.effort = Some("low".to_owned());
    let (status, body) = lab.start("/term", &explicit).await;
    assert_eq!(status, 201);
    assert_eq!(body.map(|reply| reply.command), Some("grok".to_owned()));
    assert_eq!(
        lab.spawns().last(),
        Some(&(
            vec![
                "grok".to_owned(),
                "--model".to_owned(),
                "fast".to_owned(),
                "--effort".to_owned(),
                "low".to_owned()
            ],
            normalized
        ))
    );
}

#[tokio::test]
async fn chat_starts_in_the_agent_home_outside_the_terminal_root() {
    let home = tempfile::tempdir().expect("agent home");
    let lab = Lab::open(&[preset("p-one", |p| {
        p.model = "opus".to_owned();
        p.directory = Some(directory(home.path(), "p-one"));
    })])
    .await;
    let (status, body) = http(
        &lab.gateway,
        "POST",
        "/api/harness-sessions",
        br#"{"agentId":"p-one"}"#.to_vec(),
        None,
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let summary: wire::harness::HarnessSessionSummary =
        serde_json::from_slice(&body).expect("summary");
    let started = home.path().join("p-one");
    assert_eq!(summary.harness_id, HarnessId::ClaudeCode);
    assert_eq!(summary.agent_name.as_deref(), Some("Agent p-one"));
    assert_eq!(summary.model.as_deref(), Some("opus"));
    assert_eq!(summary.cwd, started.to_str().map(str::to_owned));
    let specs = lab.harness.specs.lock().clone();
    assert_eq!(
        specs
            .iter()
            .map(|spec| spec.working_dir.clone())
            .collect::<Vec<_>>(),
        vec![Some(started)]
    );
}
