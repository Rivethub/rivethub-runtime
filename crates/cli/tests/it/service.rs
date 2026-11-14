#![cfg(test)]

use std::ffi::OsStr;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

use cli::service::{NodeKind, Scope, ServiceCommand, ServiceError, Unit};
use cli::{Cli, Dispatch, Exit};
use config::Environment;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

struct Fixture {
    home: TempDir,
    bin: PathBuf,
    calls: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        std::fs::create_dir_all(&root).expect("test directory");
        let home = tempfile::tempdir_in(root.canonicalize().expect("absolute test root"))
            .expect("temporary home");
        let bin = home.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let calls = home.path().join("calls");
        let fixture = Self { home, bin, calls };
        for name in ["systemctl", "journalctl", "loginctl"] {
            fixture.script(name, r#"#!/bin/sh
printf '%s' "${0##*/}" >> "$RIVETHUB_TEST_CALLS"
for arg do printf '|%s' "$arg" >> "$RIVETHUB_TEST_CALLS"; done
printf '\n' >> "$RIVETHUB_TEST_CALLS"
if [ "$1" = '--user' ]; then shift; fi
if [ "$1" = "$RIVETHUB_TEST_FAIL" ]; then
    printf 'operation failed\n' >&2
    exit 7
fi
case "$1" in
    status) printf 'service status\n'; printf 'status diagnostic\n' >&2; exit "${RIVETHUB_TEST_STATUS:-0}" ;;
    show) printf '%s\n' "${RIVETHUB_TEST_MAIN_PID:-0}" ;;
    show-user) printf '%s\n' "${RIVETHUB_TEST_LINGER:-yes}" ;;
esac
"#);
        }
        fixture
    }

    fn script(&self, name: &str, text: &str) {
        let path = self.bin.join(name);
        std::fs::write(&path, text).expect("fake tool");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("executable");
    }

    fn env(&self, extra: &[(&str, &str)]) -> Environment {
        Environment::from_vars(
            [
                (OsStr::new("HOME"), self.home.path().as_os_str()),
                (OsStr::new("PATH"), self.bin.as_os_str()),
                (OsStr::new("RIVETHUB_TEST_CALLS"), self.calls.as_os_str()),
            ]
            .into_iter()
            .chain(
                extra
                    .iter()
                    .map(|(key, value)| (OsStr::new(key), OsStr::new(value))),
            ),
        )
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(&self.calls).expect("tool calls")
    }

    async fn run(
        &self,
        scope: Scope,
        action: ServiceCommand,
        extra: &[(&str, &str)],
    ) -> Result<cli::Report, ServiceError> {
        tokio::time::timeout(
            Duration::from_secs(30),
            scope.run(action, &self.env(extra), &CancellationToken::new()),
        )
        .await
        .expect("command deadline")
    }
}

#[tokio::test]
async fn verbs_replay_b9_for_both_scopes() {
    for (scope, prefix) in [
        (Scope::System, "systemctl"),
        (Scope::User, "systemctl|--user"),
    ] {
        for action in [ServiceCommand::Start, ServiceCommand::Restart] {
            let fixture = Fixture::new();
            let report = fixture
                .run(scope, action, &[])
                .await
                .expect("start/restart");
            assert_eq!(report.exit, Exit::Ok);
            assert_eq!(report.stdout, "service status\n");
            assert_eq!(report.stderr, "status diagnostic\n");
            assert_eq!(
                fixture.calls(),
                format!(
                    "{prefix}|daemon-reload\n{prefix}|{action}|rivethub\n{prefix}|status|rivethub|--no-pager\n"
                )
            );
        }
        let fixture = Fixture::new();
        assert_eq!(
            fixture
                .run(scope, ServiceCommand::Stop, &[])
                .await
                .expect("stop")
                .exit,
            Exit::Ok
        );
        assert_eq!(fixture.calls(), format!("{prefix}|stop|rivethub\n"));
        for code in [0, 1, 3, 7, 255] {
            let fixture = Fixture::new();
            let report = fixture
                .run(
                    scope,
                    ServiceCommand::Status,
                    &[("RIVETHUB_TEST_STATUS", &code.to_string())],
                )
                .await
                .expect("status");
            let expected = match code {
                0 => Exit::Ok,
                1 => Exit::Failure,
                code => Exit::Child(code),
            };
            assert_eq!(report.exit, expected);
            assert_eq!(report.exit.code(), std::process::ExitCode::from(code));
            assert_eq!(report.stdout, "service status\n");
            assert_eq!(report.stderr, "status diagnostic\n");
            assert_eq!(
                fixture.calls(),
                format!("{prefix}|status|rivethub|--no-pager\n")
            );
        }
    }
}

#[tokio::test]
async fn failures_stop_the_sequence_and_reach_the_existing_exit_enum() {
    for action in [
        ServiceCommand::Start,
        ServiceCommand::Restart,
        ServiceCommand::Stop,
    ] {
        for failing in ["daemon-reload", &action.to_string()] {
            if matches!(action, ServiceCommand::Stop) && failing == "daemon-reload" {
                continue;
            }
            let fixture = Fixture::new();
            let error = fixture
                .run(Scope::User, action, &[("RIVETHUB_TEST_FAIL", failing)])
                .await
                .expect_err("failure");
            assert!(
                matches!(error, ServiceError::Failed { exit: Exit::Child(7), ref stderr } if stderr == "operation failed\n")
            );
            assert!(!fixture.calls().contains("|status|"));
            if failing == "daemon-reload" {
                assert_eq!(fixture.calls(), "systemctl|--user|daemon-reload\n");
            }
            let report = cli::Report::failure(&cli::Error::from(error));
            assert_eq!(report.exit, Exit::Failure);
            assert!(report.stderr.contains("operation failed\n"));
        }
    }
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.bin.join("systemctl")).expect("remove fake");
    assert!(
        matches!(fixture.run(Scope::User, ServiceCommand::Status, &[]).await, Err(ServiceError::Process { source, .. }) if source.kind() == std::io::ErrorKind::NotFound)
    );
}

#[tokio::test]
async fn init_reports_lingering_and_never_overwrites() {
    for (value, message) in [
        ("yes", "Lingering is enabled.\n"),
        (
            "no",
            "Lingering is disabled.\nRun: sudo loginctl enable-linger ",
        ),
    ] {
        let fixture = Fixture::new();
        let report = fixture
            .run(
                Scope::User,
                ServiceCommand::Init,
                &[("RIVETHUB_TEST_LINGER", value)],
            )
            .await
            .expect("init");
        let path = Scope::User.unit_path(fixture.home.path());
        assert!(
            report
                .stdout
                .starts_with(&format!("Created service file: {}\n", path.display()))
        );
        assert!(report.stdout.contains(message));
        assert!(report.stdout.contains("It runs: rh local\n"));
        assert!(report.stdout.contains("Enabled: it starts when you log in"));
        let contents = std::fs::read_to_string(&path).expect("unit");
        assert!(contents.contains("Type=simple\n"));
        assert!(contents.contains(" local\nEnvironmentFile="));
        assert!(
            contents.contains(
                "Environment=RIVETHUB_LOG_LEVEL=info\nRestart=on-failure\nRestartSec=5\nRestartPreventExitStatus=11\n"
            )
        );
        assert!(contents.contains("WantedBy=default.target\n"));
        let again = fixture
            .run(Scope::User, ServiceCommand::Init, &[])
            .await
            .expect("already current");
        assert!(
            again.stdout.starts_with(&format!(
                "Service file is already current: {}\n",
                path.display()
            )),
            "{}",
            again.stdout
        );
        assert_eq!(std::fs::read_to_string(&path).expect("unchanged"), contents);
        assert!(!backup_of(&path).exists());
        let sequence = format!(
            "systemctl|--user|daemon-reload\nsystemctl|--user|enable|rivethub\nloginctl|show-user|{}|--property=Linger|--value\n",
            nix::unistd::User::from_uid(nix::unistd::geteuid())
                .expect("account")
                .expect("user")
                .name
        );
        assert_eq!(fixture.calls(), format!("{sequence}{sequence}"));
    }
}

const NODE_CONFIG: &str = "runtime:\n  workspace: /home/user/work\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://127.0.0.1:1/v1\n    model: m\n";

#[tokio::test]
async fn init_runs_the_node_the_config_describes() {
    let standalone = "memory:\n  backend: sqlite\n  sqlite:\n    path: /home/user/.rivethub/memory.sqlite\ngateway:\n  host: 127.0.0.1\n  port: 5174\n";
    for (config, runs) in [(standalone, "local"), (NODE_CONFIG, "start")] {
        let fixture = Fixture::new();
        let state = fixture.home.path().join(".rivethub");
        std::fs::create_dir(&state).expect("state");
        std::fs::write(state.join("config.yaml"), config).expect("config");
        let report = fixture
            .run(Scope::User, ServiceCommand::Init, &[])
            .await
            .expect("init");
        assert!(report.stdout.contains(&format!("It runs: rh {runs}\n")));
        let unit =
            std::fs::read_to_string(Scope::User.unit_path(fixture.home.path())).expect("unit");
        assert!(
            unit.contains(&format!(" {runs}\nEnvironmentFile=")),
            "{unit}"
        );
        assert!(
            fixture
                .calls()
                .contains("systemctl|--user|enable|rivethub\n")
        );
    }
}

#[tokio::test]
async fn init_refuses_a_config_it_cannot_read_and_enables_nothing() {
    let fixture = Fixture::new();
    let state = fixture.home.path().join(".rivethub");
    std::fs::create_dir(&state).expect("state");
    std::fs::write(state.join("config.yaml"), "runtime: {}\n").expect("config");
    let error = fixture
        .run(Scope::User, ServiceCommand::Init, &[])
        .await
        .expect_err("invalid config");
    assert!(matches!(error, ServiceError::Config(_)));
    assert!(!Scope::User.unit_path(fixture.home.path()).exists());
    assert!(!fixture.calls.exists());
}

#[tokio::test]
async fn init_reports_a_failed_enable() {
    let fixture = Fixture::new();
    let error = fixture
        .run(
            Scope::User,
            ServiceCommand::Init,
            &[("RIVETHUB_TEST_FAIL", "enable")],
        )
        .await
        .expect_err("enable failure");
    assert!(
        matches!(error, ServiceError::Failed { exit: Exit::Child(7), ref stderr } if stderr == "operation failed\n")
    );
    assert!(!fixture.calls().contains("loginctl"));
}

#[test]
fn typed_unit_has_one_format_and_escapes_systemd_expansions() {
    let unit = Unit::new(
        Path::new("/opt/Rivet Hub/rh"),
        Path::new("/home/user/.rivethub/.env"),
        Scope::System,
        NodeKind::Node,
    )
    .expect("unit");
    assert_eq!(
        unit.to_string(),
        "[Unit]\nDescription=RivetHub\nAfter=network.target\n\n[Service]\nType=simple\nExecStart=\"/opt/Rivet Hub/rh\" start\nEnvironmentFile=-/home/user/.rivethub/.env\nEnvironment=RIVETHUB_LOG_LEVEL=info\nRestart=on-failure\nRestartSec=5\nRestartPreventExitStatus=11\n\n[Install]\nWantedBy=multi-user.target\n"
    );
    let unit = Unit::new(
        Path::new("/opt/é'\"\\%/$HOME/$(touch nope)/rh"),
        Path::new("/home/a%u/$HOME/.env"),
        Scope::User,
        NodeKind::Standalone,
    )
    .expect("literal paths");
    assert!(
        unit.to_string()
            .contains("ExecStart=\"/opt/é'\\\"\\\\%%/$$HOME/$$(touch nope)/rh\" local\n")
    );
    assert!(
        unit.to_string()
            .contains("EnvironmentFile=-/home/a%%u/$HOME/.env\n")
    );
    assert_eq!(Scope::for_uid(0), Scope::System);
    assert_eq!(Scope::for_uid(1000), Scope::User);
    assert_eq!(
        Scope::System.unit_path(Path::new("/home/user")),
        Path::new("/etc/systemd/system/rivethub.service")
    );
    assert_eq!(
        Scope::User.unit_path(Path::new("/home/user")),
        Path::new("/home/user/.config/systemd/user/rivethub.service")
    );
    for path in ["relative/rh", "/opt/../rh"] {
        assert!(
            Unit::new(
                Path::new(path),
                Path::new("/home/user/.env"),
                Scope::User,
                NodeKind::Node
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn init_refuses_escape_links_traversal_and_non_directories() {
    let fixture = Fixture::new();
    let sibling = fixture.home.path().with_extension("sibling");
    std::fs::create_dir(&sibling).expect("shared-prefix sibling");
    symlink(&sibling, fixture.home.path().join(".config")).expect("escape link");
    assert!(matches!(
        fixture.run(Scope::User, ServiceCommand::Init, &[]).await,
        Err(ServiceError::Path(_))
    ));
    assert_eq!(std::fs::read_dir(&sibling).expect("sibling").count(), 0);
    std::fs::remove_dir(&sibling).expect("remove sibling");
    let fixture = Fixture::new();
    let path = Scope::User.unit_path(fixture.home.path());
    std::fs::create_dir_all(path.parent().expect("parent")).expect("parent dirs");
    symlink(fixture.home.path().join("absent"), &path).expect("dangling unit");
    assert!(matches!(
        fixture.run(Scope::User, ServiceCommand::Init, &[]).await,
        Err(ServiceError::Exists(_))
    ));
    let fixture = Fixture::new();
    std::fs::write(fixture.home.path().join(".config"), "file").expect("blocking file");
    assert!(
        fixture
            .run(Scope::User, ServiceCommand::Init, &[])
            .await
            .is_err()
    );
    let unit = Unit::new(
        Path::new("/opt/rh"),
        Path::new("/home/user/.env"),
        Scope::User,
        NodeKind::Node,
    )
    .expect("unit");
    assert!(matches!(
        unit.install(fixture.home.path().join("../escape")).await,
        Err(ServiceError::Path(_))
    ));
}

#[tokio::test]
async fn dispatch_and_usage_include_the_service_tree() {
    let fixture = Fixture::new();
    let cli = Cli::parse_args(["rh", "service", "status"].map(Into::into)).expect("parse");
    let Dispatch::Complete(report) = cli::dispatch(
        cli,
        &fixture.env(&[("RIVETHUB_TEST_STATUS", "7")]),
        &CancellationToken::new(),
    )
    .await
    else {
        panic!("dispatch");
    };
    assert_eq!(report.exit, Exit::Child(7));
    for args in [
        vec!["rh", "service", "--help"],
        vec!["rh", "service", "init", "--help"],
    ] {
        assert_eq!(
            Cli::parse_args(args.into_iter().map(Into::into))
                .expect_err("help")
                .exit,
            Exit::Ok
        );
    }
    let report = Cli::parse_args(["rh", "service", "other"].map(Into::into)).expect_err("unknown");
    assert_eq!(report.exit, Exit::Failure);
    assert!(report.stderr.starts_with("Unknown action: other\n"));
    for action in ["init", "start", "stop", "restart", "status", "logs"] {
        assert!(report.stderr.contains(action));
    }
}

#[tokio::test]
async fn loginctl_errors_are_explicit() {
    let fixture = Fixture::new();
    let error = fixture
        .run(
            Scope::User,
            ServiceCommand::Init,
            &[("RIVETHUB_TEST_FAIL", "show-user")],
        )
        .await
        .expect_err("loginctl failure");
    assert!(
        matches!(error, ServiceError::Failed { exit: Exit::Child(7), stderr } if stderr == "operation failed\n")
    );
    let fixture = Fixture::new();
    assert!(matches!(
        fixture
            .run(
                Scope::User,
                ServiceCommand::Init,
                &[("RIVETHUB_TEST_LINGER", "invalid")],
            )
            .await,
        Err(ServiceError::Linger)
    ));
}

#[tokio::test]
async fn logs_follow_until_cancelled_and_reap_the_child() {
    use tokio::io::AsyncReadExt;
    let fixture = Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("readiness listener");
    fixture.script(
        "journalctl",
        r#"#!/bin/bash
printf '%s\n' "$*" > "$RIVETHUB_TEST_CALLS"
exec 3<>/dev/tcp/127.0.0.1/$RIVETHUB_TEST_PORT
printf 'ready\n' >&3
read -r line <&3
"#,
    );
    let port = listener.local_addr().expect("address").port().to_string();
    let env = fixture.env(&[("RIVETHUB_TEST_PORT", &port)]);
    let shutdown = CancellationToken::new();
    let task_shutdown = shutdown.clone();
    let task = tokio::spawn(async move {
        Scope::User
            .run(ServiceCommand::Logs, &env, &task_shutdown)
            .await
    });
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("ready deadline")
        .expect("ready connection");
    let mut ready = [0; 6];
    tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut ready))
        .await
        .expect("event deadline")
        .expect("ready event");
    assert_eq!(&ready, b"ready\n");
    shutdown.cancel();
    let report = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("shutdown deadline")
        .expect("join")
        .expect("logs");
    assert_eq!(report.exit, Exit::Ok);
    assert_eq!(fixture.calls(), "--user -u rivethub -f --no-pager\n");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), stream.read(&mut ready))
            .await
            .expect("EOF deadline")
            .expect("child closed socket"),
        0
    );
}

#[tokio::test]
async fn cancellation_before_spawn_does_not_run_tools() {
    let fixture = Fixture::new();
    let shutdown = CancellationToken::new();
    shutdown.cancel();
    for action in [
        ServiceCommand::Init,
        ServiceCommand::Start,
        ServiceCommand::Stop,
        ServiceCommand::Restart,
        ServiceCommand::Status,
    ] {
        assert!(matches!(
            Scope::User.run(action, &fixture.env(&[]), &shutdown).await,
            Err(ServiceError::Cancelled)
        ));
    }
    assert_eq!(
        Scope::User
            .run(ServiceCommand::Logs, &fixture.env(&[]), &shutdown)
            .await
            .expect("interrupted logs")
            .exit,
        Exit::Ok
    );
    assert!(!fixture.calls.try_exists().expect("calls metadata"));
}

#[tokio::test]
async fn logs_preserve_failure_and_treat_interrupt_as_success() {
    for (body, exit) in [("exit 9", Exit::Child(9)), ("kill -INT $$", Exit::Ok)] {
        let fixture = Fixture::new();
        fixture.script("journalctl", &format!("#!/bin/sh\n{body}\n"));
        assert_eq!(
            fixture
                .run(Scope::System, ServiceCommand::Logs, &[])
                .await
                .expect("logs")
                .exit,
            exit
        );
    }
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.bin.join("journalctl")).expect("remove fake");
    assert!(matches!(
        fixture.run(Scope::System, ServiceCommand::Logs, &[]).await,
        Err(ServiceError::Process { .. })
    ));
}

#[tokio::test]
async fn capture_cancellation_reaps_a_blocked_systemctl() {
    use tokio::io::AsyncReadExt;
    let fixture = Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    fixture.script(
        "systemctl",
        r"#!/bin/bash
exec 3<>/dev/tcp/127.0.0.1/$RIVETHUB_TEST_PORT
read -r line <&3
",
    );
    let env = fixture.env(&[(
        "RIVETHUB_TEST_PORT",
        &listener.local_addr().expect("address").port().to_string(),
    )]);
    let shutdown = CancellationToken::new();
    let task_shutdown = shutdown.clone();
    let task = tokio::spawn(async move {
        Scope::System
            .run(ServiceCommand::Start, &env, &task_shutdown)
            .await
    });
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("ready deadline")
        .expect("ready");
    shutdown.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("shutdown deadline")
            .expect("join"),
        Err(ServiceError::Cancelled)
    ));
    assert_eq!(stream.read(&mut [0]).await.expect("closed socket"), 0);
}

#[tokio::test]
async fn concurrent_initializers_publish_exactly_one_complete_unit() {
    let fixture = Fixture::new();
    let new_unit = || {
        Unit::new(
            Path::new("/opt/rh"),
            Path::new("/home/user/.env"),
            Scope::User,
            NodeKind::Node,
        )
        .expect("unit")
    };
    let expected = new_unit().to_string();
    let (first, second) = tokio::join!(
        new_unit().install(fixture.home.path().to_path_buf()),
        new_unit().install(fixture.home.path().to_path_buf())
    );
    assert!(matches!(
        (first, second),
        (Ok(_), Err(ServiceError::Exists(_))) | (Err(ServiceError::Exists(_)), Ok(_))
    ));
    assert_eq!(
        std::fs::read_to_string(Scope::User.unit_path(fixture.home.path())).expect("unit"),
        expected
    );
}

#[test]
fn unit_paths_preserve_unicode_and_cannot_inject_directives() {
    let unit = Unit::new(
        Path::new("/opt/\u{85}/rh"),
        Path::new("/home/user/.env"),
        Scope::User,
        NodeKind::Node,
    )
    .expect("unit");
    assert!(
        unit.to_string()
            .contains("ExecStart=\"/opt/\u{85}/rh\" start\n")
    );
    assert!(
        unit.to_string()
            .contains("EnvironmentFile=-/home/user/.env\n")
    );
    for path in [
        "/home/user/\nRestart=always.env",
        "/home/user/\t.env",
        "/home/user/\"quoted\".env",
    ] {
        let error = Unit::new(
            Path::new("/opt/rh"),
            Path::new(path),
            Scope::User,
            NodeKind::Node,
        )
        .expect_err(path);
        assert!(
            error.to_string().contains("systemd cannot represent"),
            "{path}: {error}"
        );
    }
    assert!(
        Unit::new(
            Path::new("/opt/rh\0"),
            Path::new("/home/user/.env"),
            Scope::User,
            NodeKind::Node
        )
        .is_err()
    );
}

#[test]
fn systemd_analyze_accepts_the_generated_unit() {
    let mut version = std::process::Command::new("systemd-analyze");
    version.arg("--version");
    match version.output() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipped: systemd-analyze is not installed");
            return;
        }
        Err(error) => panic!("systemd-analyze: {error}"),
        Ok(output) if !output.status.success() => {
            panic!(
                "systemd-analyze --version failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(_) => {}
    }
    let root = tempfile::tempdir().expect("unit directory");
    let executable_dir = root.path().join("Rivet Hub");
    std::fs::create_dir(&executable_dir).expect("executable directory");
    let executable = executable_dir.join("rh");
    std::fs::copy("/bin/sleep", &executable).expect("copy sleep");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).expect("mode");
    let environment = root.path().join("a%u Hub.env");
    let unit =
        Unit::new(&executable, &environment, Scope::User, NodeKind::Standalone).expect("unit");
    let text = unit.to_string();
    assert!(text.contains("EnvironmentFile=-") && !text.contains("EnvironmentFile=-\""));
    assert!(text.contains("a%%u Hub.env"));
    assert!(text.contains("ExecStart=\"") && text.contains("Rivet Hub/rh\" local\n"));
    let file = root.path().join("rivethub.service");
    std::fs::write(&file, &text).expect("unit file");
    let output = std::process::Command::new("systemd-analyze")
        .arg("verify")
        .arg(&file)
        .output()
        .expect("verify");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "systemd-analyze verify rejected the unit: {stderr}\n{text}"
    );
    assert!(
        !stderr.to_lowercase().contains("not absolute")
            && !stderr.to_lowercase().contains("ignoring"),
        "{stderr}"
    );
}

fn backup_of(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("name");
    path.with_file_name(format!("{name}.bak"))
}

fn legacy_quoted(current: &str) -> String {
    let mut out = String::new();
    for line in current
        .replacen("RestartPreventExitStatus=11\n", "", 1)
        .lines()
    {
        if let Some(path) = line.strip_prefix("EnvironmentFile=-") {
            out.push_str("EnvironmentFile=-\"");
            out.push_str(path);
            out.push_str("\"\n");
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[tokio::test]
async fn init_updates_a_generated_unit_and_keeps_the_previous_file() {
    let fixture = Fixture::new();
    fixture
        .run(Scope::User, ServiceCommand::Init, &[])
        .await
        .expect("init");
    let path = Scope::User.unit_path(fixture.home.path());
    let current = std::fs::read_to_string(&path).expect("unit");
    let legacy = legacy_quoted(&current);
    assert_ne!(legacy, current);
    std::fs::write(&path, &legacy).expect("legacy unit");
    std::fs::write(backup_of(&path), "kept\n").expect("occupied backup");
    std::fs::remove_file(&fixture.calls).expect("clear calls");
    let report = fixture
        .run(Scope::User, ServiceCommand::Init, &[])
        .await
        .expect("repair");
    let kept = path.with_file_name("rivethub.service.bak.1");
    assert_eq!(
        report.stdout,
        format!(
            "Updated {} (previous unit kept at {}): EnvironmentFile is no longer quoted; added RestartPreventExitStatus=11.\nIt runs: rh local\nEnabled: it starts when you log in, and at boot when lingering is on.\nLingering is enabled.\n",
            path.display(),
            kept.display()
        )
    );
    assert_eq!(std::fs::read_to_string(&path).expect("repaired"), current);
    assert_eq!(std::fs::read_to_string(&kept).expect("backup"), legacy);
    assert_eq!(
        std::fs::read_to_string(backup_of(&path)).expect("old bak"),
        "kept\n"
    );
    assert!(
        fixture
            .calls()
            .starts_with("systemctl|--user|daemon-reload\n")
    );
}

#[tokio::test]
async fn init_leaves_a_hand_edited_unit_and_names_the_difference() {
    let fixture = Fixture::new();
    fixture
        .run(Scope::User, ServiceCommand::Init, &[])
        .await
        .expect("init");
    let path = Scope::User.unit_path(fixture.home.path());
    let edited = std::fs::read_to_string(&path)
        .expect("unit")
        .replace("RestartSec=5\n", "RestartSec=10\n");
    std::fs::write(&path, &edited).expect("edited unit");
    std::fs::remove_file(&fixture.calls).expect("clear calls");
    let error = fixture
        .run(Scope::User, ServiceCommand::Init, &[])
        .await
        .expect_err("hand edited");
    let message = error.to_string();
    assert!(message.contains("edited by hand"), "{message}");
    assert!(message.contains("  unit: RestartSec=10"), "{message}");
    assert!(message.contains("  rh:   RestartSec=5"), "{message}");
    assert!(message.contains("`rh service init`"), "{message}");
    assert_eq!(std::fs::read_to_string(&path).expect("untouched"), edited);
    assert!(!backup_of(&path).exists());
    assert!(!fixture.calls.exists());
}

#[tokio::test]
async fn start_updates_a_generated_unit_before_it_reloads() {
    let fixture = Fixture::new();
    fixture
        .run(Scope::User, ServiceCommand::Init, &[])
        .await
        .expect("init");
    let path = Scope::User.unit_path(fixture.home.path());
    let current = std::fs::read_to_string(&path).expect("unit");
    let legacy = legacy_quoted(&current);
    std::fs::write(&path, &legacy).expect("legacy unit");
    std::fs::remove_file(&fixture.calls).expect("clear calls");
    let report = fixture
        .run(Scope::User, ServiceCommand::Start, &[])
        .await
        .expect("start");
    assert_eq!(
        report.stdout,
        format!(
            "Updated {} (previous unit kept at {}): EnvironmentFile is no longer quoted; added RestartPreventExitStatus=11.\nservice status\n",
            path.display(),
            backup_of(&path).display()
        )
    );
    assert_eq!(std::fs::read_to_string(&path).expect("repaired"), current);
    assert_eq!(
        std::fs::read_to_string(backup_of(&path)).expect("backup"),
        legacy
    );
    assert_eq!(
        fixture.calls(),
        "systemctl|--user|daemon-reload\nsystemctl|--user|start|rivethub\nsystemctl|--user|status|rivethub|--no-pager\n"
    );
}

#[tokio::test]
async fn start_names_a_hand_edited_unit_and_still_starts_it() {
    let fixture = Fixture::new();
    fixture
        .run(Scope::User, ServiceCommand::Init, &[])
        .await
        .expect("init");
    let path = Scope::User.unit_path(fixture.home.path());
    let edited = std::fs::read_to_string(&path)
        .expect("unit")
        .replace("RestartSec=5\n", "RestartSec=10\n");
    std::fs::write(&path, &edited).expect("edited unit");
    std::fs::remove_file(&fixture.calls).expect("clear calls");
    let report = fixture
        .run(Scope::User, ServiceCommand::Restart, &[])
        .await
        .expect("restart");
    assert!(
        report.stdout.contains("edited by hand"),
        "{}",
        report.stdout
    );
    assert!(
        report.stdout.contains("  unit: RestartSec=10"),
        "{}",
        report.stdout
    );
    assert!(
        report.stdout.ends_with("service status\n"),
        "{}",
        report.stdout
    );
    assert_eq!(std::fs::read_to_string(&path).expect("untouched"), edited);
    assert!(!backup_of(&path).exists());
    assert_eq!(
        fixture.calls(),
        "systemctl|--user|daemon-reload\nsystemctl|--user|restart|rivethub\nsystemctl|--user|status|rivethub|--no-pager\n"
    );
}

fn hold_pid(home: &Path, program: &str) -> std::process::Child {
    let state = home.join(".rivethub");
    std::fs::create_dir_all(&state).expect("state");
    let ready = state.join("holder.ready");
    let child = std::process::Command::new("/usr/bin/python3")
        .arg("-c")
        .arg(program)
        .env("READY", &ready)
        .spawn()
        .expect("holder");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !ready.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "holder did not become ready"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(20));
    }
    std::fs::write(state.join("rivethub.pid"), format!("{}\n", child.id())).expect("pid");
    child
}

const SLEEPING_HOLDER: &str = "import os, signal\nsignal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGTERM})\nsignal.signal(signal.SIGTERM, signal.SIG_DFL)\nopen(os.environ['READY'], 'w').close()\nos.execv('/bin/sleep', ['sleep', '60'])\n";

const STUBBORN_HOLDER: &str = "import os, signal, time\nsignal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGTERM})\nsignal.signal(signal.SIGTERM, signal.SIG_IGN)\nopen(os.environ['READY'], 'w').close()\nwhile True:\n    time.sleep(1)\n";

#[tokio::test]
async fn service_start_stops_a_detached_node_before_the_unit() {
    let fixture = Fixture::new();
    let mut child = hold_pid(fixture.home.path(), SLEEPING_HOLDER);
    let pid = child.id();
    let reaper = std::thread::spawn(move || child.wait());
    let report = fixture
        .run(Scope::User, ServiceCommand::Start, &[])
        .await
        .expect("start");
    assert_eq!(
        report.stdout,
        format!(
            "Stopped the running RivetHub node (pid {pid}) so the service can take its place.\nservice status\n"
        )
    );
    assert_eq!(
        fixture.calls(),
        "systemctl|--user|show|-p|MainPID|--value|rivethub\nsystemctl|--user|daemon-reload\nsystemctl|--user|start|rivethub\nsystemctl|--user|status|rivethub|--no-pager\n"
    );
    let finished = reaper.join().expect("reaper").expect("wait");
    assert!(!finished.success());
}

#[tokio::test]
async fn service_start_leaves_the_units_own_process_running() {
    let fixture = Fixture::new();
    let mut child = hold_pid(fixture.home.path(), SLEEPING_HOLDER);
    let pid = child.id().to_string();
    let report = fixture
        .run(
            Scope::User,
            ServiceCommand::Restart,
            &[("RIVETHUB_TEST_MAIN_PID", &pid)],
        )
        .await
        .expect("restart");
    assert_eq!(report.stdout, "service status\n");
    assert!(child.try_wait().expect("poll").is_none());
    child.kill().expect("stop holder");
    child.wait().expect("reap");
}

#[tokio::test]
async fn service_start_refuses_once_when_the_detached_node_stays_up() {
    let fixture = Fixture::new();
    let mut child = hold_pid(fixture.home.path(), STUBBORN_HOLDER);
    let pid = child.id();
    let error = fixture
        .run(Scope::User, ServiceCommand::Start, &[])
        .await
        .expect_err("still running");
    assert!(
        matches!(error, ServiceError::Running { pid: found } if found == pid),
        "{error}"
    );
    assert_eq!(
        fixture.calls(),
        "systemctl|--user|show|-p|MainPID|--value|rivethub\n"
    );
    child.kill().expect("stop holder");
    child.wait().expect("reap");
}
