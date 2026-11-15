#![cfg(test)]

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cli::Exit;
use cli::service::Scope;
use config::Environment;
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
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
        let home =
            tempfile::tempdir_in(root.canonicalize().expect("absolute test root")).expect("home");
        let bin = home.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let calls = home.path().join("calls");
        let fixture = Self { home, bin, calls };
        fixture.script(
            "journalctl",
            r#"#!/bin/sh
printf '%s' "${0##*/}" >> "$RIVETHUB_TEST_CALLS"
for arg do printf '|%s' "$arg" >> "$RIVETHUB_TEST_CALLS"; done
printf '\n' >> "$RIVETHUB_TEST_CALLS"
"#,
        );
        fixture
    }

    fn script(&self, name: &str, text: &str) {
        let path = self.bin.join(name);
        std::fs::write(&path, text).expect("fake tool");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("executable");
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
                    .map(|(key, value)| (OsStr::new(*key), OsStr::new(*value))),
            ),
        )
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(&self.calls).expect("tool calls")
    }

    fn spawned(&self) -> bool {
        self.calls.is_file()
    }
}

fn prefix(scope: Scope) -> &'static str {
    match scope {
        Scope::User => "journalctl|--user|-u|rivethub",
        Scope::System => "journalctl|-u|rivethub",
    }
}

async fn dispatch(parts: &[&str], environment: &Environment) -> cli::Report {
    let parsed = cli::Cli::parse_args(
        std::iter::once("rh")
            .chain(parts.iter().copied())
            .map(Into::into),
    );
    let cli = match parsed {
        Ok(cli) => cli,
        Err(report) => return report,
    };
    let output = cli::dispatch(cli, environment, &CancellationToken::new()).await;
    let cli::Dispatch::Complete(report) = output else {
        panic!("unexpected service start")
    };
    report
}

async fn logs(scope: Scope, parts: &[&str], environment: &Environment) -> cli::Report {
    let owned: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
    cli::logs::execute(scope, &owned, environment, &CancellationToken::new())
        .await
        .expect("logs")
}

#[tokio::test]
async fn logs_help_and_unknown_options_do_not_read_the_journal() {
    let fixture = Fixture::new();
    let environment = fixture.env(&[]);
    let help = dispatch(&["logs", "--help"], &environment).await;
    assert_eq!(help.exit, Exit::Ok);
    assert!(help.stdout.contains("Usage: rh logs"));
    assert!(!help.stdout.contains("doctor"));
    let unknown = dispatch(&["logs", "--bogus", "--no-follow"], &environment).await;
    assert_eq!(unknown.exit, Exit::Failure);
    assert!(unknown.stderr.starts_with("Unknown option: --bogus\n"));
    assert!(unknown.stderr.contains("Usage: rh logs"));
    assert!(!unknown.stderr.starts_with("Error:"));
    let two = dispatch(&["logs", "main", "other", "--no-follow"], &environment).await;
    assert_eq!(two.stderr, "Error: rh logs accepts one agent identifier\n");
    assert!(!fixture.spawned());
}

#[tokio::test]
async fn logs_reads_the_rivethub_journal_for_both_scopes() {
    for scope in [Scope::User, Scope::System] {
        let fixture = Fixture::new();
        let report = logs(scope, &["--no-follow"], &fixture.env(&[])).await;
        assert_eq!(report.exit, Exit::Ok);
        assert_eq!(
            fixture.calls(),
            format!("{}|-n|50|--no-pager\n", prefix(scope))
        );
        let fixture = Fixture::new();
        let report = logs(
            scope,
            &[
                "main",
                "--no-follow",
                "--lines=10",
                "--since",
                "yesterday",
                "--level",
                "err",
                "--grep",
                "boom",
                "--json",
                "--bare-metal",
            ],
            &fixture.env(&[]),
        )
        .await;
        assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
        assert_eq!(
            fixture.calls(),
            format!(
                "{}|-t|main|-n|10|--since|yesterday|-p|err|-g|boom|-o|json|--no-pager\n",
                prefix(scope)
            )
        );
    }
    let fixture = Fixture::new();
    let report = logs(
        Scope::User,
        &["--no-follow", "-n50", "-ghello", "--level", "4"],
        &fixture.env(&[]),
    )
    .await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(
        fixture.calls(),
        format!("{}|-n|50|-p|4|-g|hello|--no-pager\n", prefix(Scope::User))
    );
    let bad = dispatch(
        &["logs", "--no-follow", "--level", "nope"],
        &fixture.env(&[]),
    )
    .await;
    assert_eq!(bad.stderr, "Error: unknown syslog level: nope\n");
    assert!(!fixture.calls().contains("nope"));
}

#[tokio::test]
async fn dispatch_uses_the_caller_scope() {
    let fixture = Fixture::new();
    let report = dispatch(&["logs", "--no-follow"], &fixture.env(&[])).await;
    let scope = Scope::for_uid(nix::unistd::geteuid().as_raw());
    assert_eq!(
        fixture.calls(),
        format!("{}|-n|50|--no-pager\n", prefix(scope))
    );
    assert_eq!(report.stderr, "");
}

#[tokio::test]
async fn follow_stops_when_cancelled_for_both_scopes() {
    for scope in [Scope::User, Scope::System] {
        let fixture = Fixture::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let port = listener.local_addr().expect("address").port().to_string();
        fixture.script(
            "journalctl",
            r#"#!/bin/bash
printf '%s\n' "$*" > "$RIVETHUB_TEST_CALLS"
exec 3<>/dev/tcp/127.0.0.1/$RIVETHUB_TEST_PORT
printf 'ready\n' >&3
read -r line <&3
"#,
        );
        let environment = fixture.env(&[("RIVETHUB_TEST_PORT", &port)]);
        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            cli::logs::execute(scope, &[], &environment, &task_shutdown).await
        });
        let mut stream = ready(&listener).await;
        shutdown.cancel();
        let report = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("shutdown deadline")
            .expect("join")
            .expect("logs");
        assert_eq!(report.exit, Exit::Ok);
        assert_eq!(report.stderr, "");
        let recorded = match scope {
            Scope::User => "--user -u rivethub -n 50 -f --no-pager\n",
            Scope::System => "-u rivethub -n 50 -f --no-pager\n",
        };
        assert_eq!(fixture.calls(), recorded);
        let mut buf = [0; 4];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
                .await
                .expect("eof deadline")
                .expect("closed"),
            0
        );
    }
}

async fn ready(listener: &tokio::net::TcpListener) -> tokio::net::TcpStream {
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("ready deadline")
        .expect("ready connection");
    let mut buf = [0; 6];
    tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut buf))
        .await
        .expect("event deadline")
        .expect("ready event");
    assert_eq!(&buf, b"ready\n");
    stream
}
