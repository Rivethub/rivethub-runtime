use crate::support;

use cli::{ConfigCommand, Exit};
use support::{config, environment, home, run};

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
    let config::ConfigError::Validation { result } =
        config::Config::load(&env, &config::ConfigLocation::Current(path.clone()))
            .expect_err("invalid")
    else {
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
