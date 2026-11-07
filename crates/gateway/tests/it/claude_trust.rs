#![cfg(test)]
use crate::session_support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gateway::tenancy::Tenancy;
use gateway::term::{ClaudeTrust, LaunchCommand, TerminalOptions, TerminalService};
use serde::{Deserialize, Serialize};
use session_support::{Call, Mux, http, user};
use wire::gateway::term::TermSpawnRequest;
use wire::harness::HarnessId;

struct Lab {
    gateway: Arc<gateway::Gateway>,
    mux: Arc<Mux>,
    root: tempfile::TempDir,
    config: PathBuf,
}

impl Lab {
    async fn open(trust: Option<&[&str]>) -> Self {
        let root = tempfile::tempdir().expect("lab directory");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside");
        let config = root.path().join(".claude.json");
        let registry = Arc::new(gateway::sessions::SessionRegistry::new(
            Duration::from_secs(1),
        ));
        let mux = Arc::new(Mux::default());
        let commands = [
            ("shell", None, &workspace),
            ("claude", Some(HarnessId::ClaudeCode), &workspace),
            ("claude-outside", Some(HarnessId::ClaudeCode), &outside),
        ]
        .into_iter()
        .map(|(id, harness, cwd)| LaunchCommand {
            id: id.into(),
            label: id.into(),
            argv: vec![id.into()],
            cwd: cwd.clone(),
            harness,
            session: harness.is_some(),
            env: BTreeMap::new(),
        })
        .collect();
        let mut service = TerminalService::new(
            mux.clone(),
            registry.clone(),
            TerminalOptions {
                roots: vec![workspace],
                default: "shell".into(),
                commands,
                max_ptys: 8,
            },
        )
        .await
        .expect("terminal service");
        if let Some(roots) = trust {
            service = service.with_claude_trust(ClaudeTrust {
                config: config.clone(),
                roots: roots.iter().map(|name| root.path().join(name)).collect(),
            });
        }
        let gateway = Arc::new(
            gateway::Gateway::new(
                Arc::new(testkit::FakeMemoryStore::default()),
                settings(root.path()),
            )
            .with_tenancy(Tenancy::local(user("owner")))
            .with_sessions(registry)
            .with_terminal(Arc::new(service)),
        );
        Self {
            gateway,
            mux,
            root,
            config,
        }
    }

    fn directory(&self, name: &str) -> String {
        self.root
            .path()
            .join(name)
            .canonicalize()
            .expect("lab directory")
            .to_str()
            .expect("utf-8 path")
            .to_owned()
    }

    async fn spawn(&self, command: &str) -> u16 {
        let body = TermSpawnRequest {
            command: Some(command.to_owned()),
            model: None,
            effort: None,
            session: Some(format!("{command}-run")),
            resume: None,
            agent_id: None,
            force: None,
            cols: None,
            rows: None,
            topic_id: None,
        };
        http(
            &self.gateway,
            "POST",
            "/api/terminal",
            serde_json::to_vec(&body).expect("body"),
            None,
        )
        .await
        .0
    }

    fn spawned(&self) -> usize {
        self.mux
            .calls
            .borrow()
            .iter()
            .filter(|call| matches!(call, Call::Spawn { .. }))
            .count()
    }

    fn stored(&self) -> Stored {
        serde_json::from_slice(&std::fs::read(&self.config).expect("claude config"))
            .expect("claude config json")
    }
}

fn settings(root: &Path) -> gateway::Settings {
    gateway::Settings {
        hostname: "gold-host".into(),
        node: "golden-node".into(),
        allowed_hosts: vec![],
        allowed_origins: vec![],
        files_root: None,
        static_dir: None,
        root_redirect: None,
        state_dir: root.join("state"),
        directory_root: root.join("agents"),
        shared_dir: root.join("shared"),
        allowed_harnesses: None,
        probe_timeout: Duration::from_millis(200),
        private_ca: None,
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    #[serde(skip_serializing_if = "Option::is_none")]
    num_startups: Option<u32>,
    projects: BTreeMap<String, Project>,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Project {
    #[serde(skip_serializing_if = "Option::is_none")]
    has_trust_dialog_accepted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_completed_project_onboarding: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_tools: Option<Vec<String>>,
}

fn trusted() -> Project {
    Project {
        has_trust_dialog_accepted: Some(true),
        has_completed_project_onboarding: Some(true),
        allowed_tools: None,
    }
}

const ON: Option<&[&str]> = Some(&[]);

#[tokio::test]
async fn by_default_a_claude_spawn_leaves_the_claude_config_alone() {
    let lab = Lab::open(None).await;
    assert_eq!(lab.spawn("claude").await, 201);
    assert!(!lab.config.exists());
    std::fs::write(&lab.config, b"{\"numStartups\": 3}").expect("write");
    assert_eq!(lab.spawn("claude-outside").await, 201);
    assert_eq!(
        std::fs::read(&lab.config).expect("config"),
        b"{\"numStartups\": 3}"
    );
}

#[tokio::test]
async fn claude_spawn_under_the_terminal_root_trusts_its_working_directory() {
    let lab = Lab::open(ON).await;
    assert_eq!(lab.spawn("claude").await, 201);
    assert_eq!(
        lab.stored(),
        Stored {
            num_startups: None,
            projects: BTreeMap::from([(lab.directory("workspace"), trusted())]),
        }
    );
    assert_eq!(lab.spawned(), 1);
}

#[tokio::test]
async fn claude_spawn_outside_the_terminal_root_is_not_trusted() {
    let lab = Lab::open(ON).await;
    assert_eq!(lab.spawn("claude-outside").await, 201);
    assert!(!lab.config.exists());
    assert_eq!(lab.spawned(), 1);
}

#[tokio::test]
async fn claude_spawn_under_a_harness_directory_is_trusted() {
    let lab = Lab::open(Some(&["outside"])).await;
    assert_eq!(lab.spawn("claude-outside").await, 201);
    assert_eq!(
        lab.stored().projects,
        BTreeMap::from([(lab.directory("outside"), trusted())])
    );
}

#[tokio::test]
async fn trust_keeps_every_other_setting_and_project() {
    let lab = Lab::open(ON).await;
    let other = Project {
        has_trust_dialog_accepted: None,
        has_completed_project_onboarding: None,
        allowed_tools: Some(vec!["Read".to_owned()]),
    };
    let existing = Stored {
        num_startups: Some(7),
        projects: BTreeMap::from([
            ("/home/user/other".to_owned(), other),
            (
                lab.directory("workspace"),
                Project {
                    has_trust_dialog_accepted: Some(false),
                    has_completed_project_onboarding: None,
                    allowed_tools: Some(vec!["Bash".to_owned()]),
                },
            ),
        ]),
    };
    std::fs::write(&lab.config, serde_json::to_vec(&existing).expect("seed")).expect("write");
    assert_eq!(lab.spawn("claude").await, 201);
    let stored = lab.stored();
    assert_eq!(stored.num_startups, Some(7));
    assert_eq!(
        stored.projects.get("/home/user/other"),
        existing.projects.get("/home/user/other")
    );
    assert_eq!(
        stored.projects.get(&lab.directory("workspace")),
        Some(&Project {
            allowed_tools: Some(vec!["Bash".to_owned()]),
            ..trusted()
        })
    );
}

#[tokio::test]
async fn an_already_trusted_directory_is_not_rewritten() {
    let lab = Lab::open(ON).await;
    let existing = Stored {
        num_startups: None,
        projects: BTreeMap::from([(lab.directory("workspace"), trusted())]),
    };
    let seeded = serde_json::to_vec(&existing).expect("seed");
    std::fs::write(&lab.config, &seeded).expect("write");
    assert_eq!(lab.spawn("claude").await, 201);
    assert_eq!(std::fs::read(&lab.config).expect("config"), seeded);
}

#[tokio::test]
async fn other_commands_leave_the_claude_config_alone() {
    let lab = Lab::open(ON).await;
    assert_eq!(lab.spawn("shell").await, 201);
    assert!(!lab.config.exists());
    assert_eq!(lab.spawned(), 1);
}

#[tokio::test]
async fn an_unreadable_claude_config_refuses_the_spawn_and_is_kept() {
    let lab = Lab::open(ON).await;
    std::fs::write(&lab.config, b"{ not json").expect("write");
    assert_eq!(lab.spawn("claude").await, 503);
    assert_eq!(std::fs::read(&lab.config).expect("config"), b"{ not json");
    assert_eq!(lab.spawned(), 0);
}
