#![cfg(test)]

use std::ffi::OsStr;

use cli::{Cli, Dispatch, Exit, Report};
use config::Environment;
use tokio_util::sync::CancellationToken;

async fn run(home: &std::path::Path, args: &[&str]) -> Report {
    let env = Environment::from_vars([
        (OsStr::new("HOME"), home.as_os_str()),
        (OsStr::new("PATH"), OsStr::new("/nonexistent")),
    ]);
    let cli = Cli::parse_args(
        std::iter::once("rh")
            .chain(args.iter().copied())
            .map(Into::into),
    )
    .expect("parse");
    let Dispatch::Complete(report) = cli::dispatch(cli, &env, &CancellationToken::new()).await
    else {
        panic!("harness commands complete in place");
    };
    report
}

#[tokio::test]
async fn connect_names_a_skipped_part_instead_of_saying_connected() {
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir(home.path().join(".cursor")).expect("cursor store");
    let report = run(home.path(), &["harness", "connect", "cursor"]).await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(
        report.stdout.ends_with("connected recall only.\n"),
        "{}",
        report.stdout
    );
    assert!(
        report
            .stderr
            .starts_with("Skipped: cursor has no capture setup rh can write: "),
        "{}",
        report.stderr
    );
    let dry = tempfile::tempdir().expect("home");
    std::fs::create_dir(dry.path().join(".cursor")).expect("cursor store");
    let preview = run(dry.path(), &["harness", "connect", "cursor", "--dry-run"]).await;
    assert!(preview.stdout.ends_with("Dry run: nothing was written.\n"));
    assert!(!dry.path().join(".cursor/mcp.json").exists());
    let only = run(home.path(), &["harness", "connect", "cursor", "--capture"]).await;
    assert_eq!(only.exit, Exit::Failure);
    assert!(!only.stdout.contains("connected"));
}

#[tokio::test]
async fn connect_opencode_installs_the_embedded_capture_plugin() {
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir_all(home.path().join(".config/opencode")).expect("opencode config");
    let report = run(home.path(), &["harness", "connect", "opencode"]).await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(report.stderr, "");
    assert!(
        report.stdout.ends_with("\nconnected.\n"),
        "{}",
        report.stdout
    );
    let plugin = std::fs::read_to_string(
        home.path()
            .join(".config/opencode/plugins/rivethub-capture.ts"),
    )
    .expect("plugin");
    assert!(plugin.contains("capture hook --harness opencode"));
    let status = run(home.path(), &["harness", "status"]).await;
    assert!(
        status
            .stdout
            .contains("capture (plugin): plug-in installed"),
        "{}",
        status.stdout
    );
}
