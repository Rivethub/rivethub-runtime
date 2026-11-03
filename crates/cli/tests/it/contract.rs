use crate::support;

use cli::{ConfigCommand, Exit};
use std::collections::BTreeSet;
use support::{config, environment, home, run};

const DEFERRED: [(&str, &str); 10] = [
    (
        "start",
        "The third slice of this contract supervises the service roots.",
    ),
    ("update --mesh", "Fleet updates are a later slice."),
    ("init", "Interactive setup is a later slice."),
    ("pair", "Device pairing CLI is a later slice."),
    ("user", "User administration is a later slice."),
    ("test", "Provider diagnostics are a later slice."),
    ("agent", "Agent commands are a later slice."),
    ("model", "Model commands are a later slice."),
    ("keys", "Key management is a later slice."),
    ("gateway token", "Gateway credentials are a later slice."),
];

#[test]
fn every_readme_deferred_scenario_has_a_named_reason() {
    let readme =
        std::fs::read_to_string(testkit::recording("cli/README.md")).expect("CLI contract");
    let deferred = readme
        .split("Out of this slice (later slices):")
        .nth(1)
        .expect("deferred scenarios")
        .split("## `rh service`")
        .next()
        .expect("deferred paragraph");
    let names: BTreeSet<_> = deferred
        .split('`')
        .enumerate()
        .filter_map(|(index, word)| (index % 2 == 1).then_some(word))
        .collect();
    let listed: BTreeSet<_> = DEFERRED
        .iter()
        .map(|(name, reason)| {
            assert_ne!(*reason, "");
            *name
        })
        .collect();
    assert_eq!(listed.len(), DEFERRED.len());
    assert_eq!(listed, names);
    let files: Vec<_> = std::fs::read_dir(testkit::recording("cli"))
        .expect("CLI recordings")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(
        files,
        ["README.md"],
        "new transcripts must be added to the replay suite"
    );
}

const SKIPPED: [(&str, &str); 4] = [
    (
        "docker log detection",
        "rh logs always reads the rivethub journal",
    ),
    (
        "bare-metal tee instructions",
        "bare metal still reads the rivethub journal",
    ),
    (
        "embedded postgres",
        "memory commands never start a database",
    ),
    (
        "rh cloud import",
        "cloud-host import exits 2; the cloud import command is not implemented",
    ),
];

#[test]
fn skipped_memory_and_logs_scenarios_name_a_reason() {
    let readme =
        std::fs::read_to_string(testkit::recording("cli/README.md")).expect("CLI contract");
    let rest = readme
        .split("## `rh service`")
        .nth(1)
        .expect("service section");
    for (name, reason) in SKIPPED {
        assert!(!reason.is_empty(), "{name}");
        assert!(rest.contains(name), "{name}");
    }
}

#[tokio::test]
async fn dispatch_help_unknown_flags_and_version_follow_b9() {
    let home = home();
    let env = environment(&home);
    for args in [vec![], vec!["help"], vec!["--help"], vec!["-h"]] {
        let report = run(&args, &env).await;
        assert_eq!(report.exit, Exit::Ok, "{args:?}");
        assert_eq!(report.stderr, "");
        for name in ["version", "config", "doctor", "status", "stop"] {
            assert!(report.stdout.contains(name));
        }
    }
    for args in [
        vec!["config", "validate", "--help"],
        vec!["doctor", "--help"],
        vec!["stop", "--help"],
    ] {
        assert_eq!(run(&args, &env).await.exit, Exit::Ok);
    }
    let unknown = run(&["not-a-command"], &env).await;
    assert_eq!(unknown.exit, Exit::Failure);
    assert_eq!(unknown.stdout, "");
    assert!(
        unknown
            .stderr
            .starts_with("Unknown command: not-a-command\n")
    );
    assert!(unknown.stderr.contains("Commands:"));
    for args in [
        vec!["doctor", "--bogus"],
        vec!["stop", "extra"],
        vec!["config", "nope"],
    ] {
        assert_eq!(run(&args, &env).await.exit, Exit::Failure);
    }
    let version = run(&["version"], &env).await;
    assert_eq!(version.exit, Exit::Ok);
    assert!(
        version
            .stdout
            .starts_with(concat!("RivetHub v", env!("CARGO_PKG_VERSION")))
    );
    assert!(
        version.stdout.ends_with(")\n"),
        "checkout builds carry a revision"
    );
}

#[tokio::test]
async fn config_path_show_and_validate_use_config_ownership() {
    let home = home();
    let env = environment(&home);
    let absent = run(&["config", "path"], &env).await;
    assert_eq!(absent.exit, Exit::Failure);
    assert_eq!(
        absent.stderr,
        "Error: No config found. Write ~/.rivethub/config.yaml (rh local writes a standalone one) or set RIVETHUB_CONFIG\n"
    );
    config(&home, "");
    let path = home.path().join(".rivethub/config.yaml");
    assert_eq!(
        run(&["config", "path"], &env).await.stdout,
        format!("{}\n", path.display())
    );
    let shown = run(&["config", "show"], &env).await;
    assert_eq!(shown.exit, Exit::Ok);
    assert_eq!(
        shown.stdout,
        "Agents:\n  main → p\nProviders:\n  p → m\nDefault agent: main\nWorkspace: /home/user/work\n"
    );
    assert!(!format!("{}{}", shown.stdout, shown.stderr).contains("secret-that-must-stay-private"));
    assert_eq!(
        run(&["config", "validate"], &env).await.stdout,
        "✅ Config is valid.\n"
    );
    assert_eq!(
        run(&["config", "validate", path.to_str().expect("path")], &env)
            .await
            .exit,
        Exit::Ok
    );
    std::fs::write(&path, "runtime: [").expect("malformed yaml");
    let malformed = run(&["config", "show"], &env).await;
    assert_eq!(malformed.exit, Exit::Failure);
    assert!(
        malformed
            .stderr
            .starts_with("Error: Failed to parse config: ")
    );
    std::fs::write(&path, "runtime: {}\n").expect("invalid configuration");
    let invalid = run(&["config", "validate"], &env).await;
    assert_eq!(invalid.exit, Exit::Failure);
    let config::ConfigError::Validation { result } = config::Config::load(
        &env,
        &config::ConfigLocation::Current(path.clone()),
        config::Profile::Node,
    )
    .expect_err("invalid") else {
        panic!("wrong error")
    };
    assert_eq!(invalid.stdout, format!("{result}\n"));
    let missing = path.with_file_name("missing.yaml");
    assert_eq!(
        run(
            &["config", "validate", missing.to_str().expect("path")],
            &env
        )
        .await
        .exit,
        Exit::Failure
    );
    let typed = cli::Cli::parse_args(["rh", "config", "validate", "explicit.yaml"].map(Into::into))
        .expect("typed tree");
    assert!(matches!(
        typed.command,
        Some(cli::Command::Config {
            command: ConfigCommand::Validate { path: Some(_) }
        })
    ));
}

#[tokio::test]
async fn cancellation_is_explicit() {
    let home = home();
    let env = environment(&home);
    let shutdown = tokio_util::sync::CancellationToken::new();
    shutdown.cancel();
    let cli = cli::Cli::parse_args(["rh", "doctor"].map(Into::into)).expect("command");
    let cli::Dispatch::Complete(report) = cli::dispatch(cli, &env, &shutdown).await else {
        panic!("service")
    };
    assert_eq!(report.exit, Exit::Failure);
    assert!(report.stderr.contains("cancelled"));
}

#[tokio::test]
async fn config_show_resolves_substitutions_and_paths_with_spaces() {
    let home = home();
    config(&home, "");
    let path = home.path().join(".rivethub/config.yaml");
    let text = std::fs::read_to_string(&path)
        .expect("config")
        .replace("/home/user/work", "'${WORKSPACE}'");
    std::fs::write(&path, text).expect("substitution");
    let env = config::Environment::from_vars([
        ("HOME".as_ref(), home.path().as_os_str()),
        ("WORKSPACE".as_ref(), "/home/user/work with spaces".as_ref()),
    ]);
    let report = run(&["config", "show"], &env).await;
    assert_eq!(report.exit, Exit::Ok);
    assert!(
        report
            .stdout
            .contains("Workspace: /home/user/work with spaces\n")
    );
    let explicit = home.path().join("quoted config.yaml");
    std::fs::copy(path, &explicit).expect("explicit config");
    assert_eq!(
        run(
            &["config", "validate", explicit.to_str().expect("path")],
            &env
        )
        .await
        .exit,
        Exit::Ok
    );
}
