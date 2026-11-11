use crate::support;

use cli::{Cli, Command, Dispatch, Exit};
use support::{config, environment, home, run};

const SKIPPED: &[(&str, &str)] = &[];

#[test]
fn skipped_local_scenarios_are_named() {
    let readme =
        std::fs::read_to_string(testkit::recording("cli/README.md")).expect("CLI contract");
    let section = readme.split("## `rh local`").nth(1).expect("local section");
    let mut listed = std::collections::BTreeMap::new();
    for line in section.lines() {
        let Some(rest) = line.trim().strip_prefix("- `") else {
            continue;
        };
        let Some((name, reason)) = rest.split_once("` — ") else {
            continue;
        };
        listed.insert(name.to_owned(), reason.trim().to_owned());
    }
    let expected: std::collections::BTreeMap<_, _> = SKIPPED
        .iter()
        .copied()
        .map(|(name, reason)| (name.to_owned(), reason.to_owned()))
        .collect();
    assert_eq!(listed, expected);
}

#[test]
fn clap_parses_port_and_no_lan_and_rejects_unknown_flags() {
    let parsed =
        Cli::parse_args(["rh", "local", "--port", "9", "--no-lan"].map(Into::into)).expect("flags");
    assert!(matches!(
        parsed.command,
        Some(Command::Local {
            port: Some(9),
            no_lan: true,
            ensure: false,
            wait: 2,
            json: false,
            no_cloud: false,
        })
    ));
    let unknown = Cli::parse_args(["rh", "local", "--yes"].map(Into::into)).expect_err("flag");
    assert_eq!(unknown.exit, Exit::Failure);
    assert!(!unknown.stderr.starts_with("Unknown command:"));
}

#[test]
fn clap_parses_ensure_with_a_bounded_wait() {
    let parsed = Cli::parse_args(
        [
            "rh", "local", "--ensure", "--wait", "7", "--port", "9", "--no-lan",
        ]
        .map(Into::into),
    )
    .expect("flags");
    assert!(matches!(
        parsed.command,
        Some(Command::Local {
            port: Some(9),
            no_lan: true,
            ensure: true,
            wait: 7,
            json: false,
            no_cloud: false,
        })
    ));
    let alone = Cli::parse_args(["rh", "local", "--wait", "7"].map(Into::into)).expect_err("wait");
    assert_eq!(alone.exit, Exit::Failure);
    assert!(alone.stderr.contains("--ensure"), "{}", alone.stderr);
    let json = Cli::parse_args(["rh", "local", "--json"].map(Into::into)).expect_err("json");
    assert_eq!(json.exit, Exit::Failure);
    assert!(json.stderr.contains("--ensure"), "{}", json.stderr);
    let zero = Cli::parse_args(["rh", "local", "--ensure", "--wait", "0"].map(Into::into))
        .expect_err("zero wait");
    assert_eq!(zero.exit, Exit::Failure);
}

#[tokio::test]
async fn a_missing_config_is_written_without_operator_sections() {
    let home = home();
    let env = environment(&home);
    let cli =
        Cli::parse_args(["rh", "local", "--no-lan", "--port", "9"].map(Into::into)).expect("parse");
    let dispatch = cli::dispatch(cli, &env, &tokio_util::sync::CancellationToken::new()).await;
    let Dispatch::Local(prepared) = dispatch else {
        panic!("local did not prepare");
    };
    let text = std::fs::read_to_string(home.path().join(".rivethub/config.yaml")).expect("config");
    assert!(text.contains("backend: sqlite"), "{text}");
    assert!(text.contains("127.0.0.1"), "{text}");
    assert!(text.contains("port: 9"), "{text}");
    assert!(!text.contains("runtime"));
    assert!(!text.contains("agents"));
    assert!(!text.contains("providers"));
    assert!(!text.contains("base_url"));
    assert!(prepared.database.ends_with("memory.sqlite"));
    assert!(prepared.database.is_file());
    assert_eq!(
        prepared.config.gateway.as_ref().map(|section| section.port),
        Some(9)
    );
    assert_eq!(
        prepared
            .config
            .gateway
            .as_ref()
            .and_then(|section| section.host.as_deref()),
        Some("127.0.0.1")
    );
    let loaded = config::Config::load(
        &env,
        &config::ConfigLocation::Current(home.path().join(".rivethub/config.yaml")),
        config::Profile::Node,
    );
    assert!(
        loaded.is_err(),
        "operator validation still requires sections"
    );
    prepared.release().expect("release");
    assert!(!home.path().join(".rivethub/rivethub.pid").exists());
}

#[tokio::test]
async fn postgres_and_a_missing_sqlite_path_exit_1() {
    let home = home();
    let env = environment(&home);
    config(
        &home,
        "memory:\n  postgres:\n    connection_string: postgres://127.0.0.1:1/none\n",
    );
    let postgres = run(&["local", "--no-lan"], &env).await;
    assert_eq!(postgres.exit, Exit::Failure);
    assert!(
        postgres
            .stderr
            .contains("rh local runs on SQLite; this config selects Postgres"),
        "{}",
        postgres.stderr
    );
    config(&home, "");
    let missing = run(&["local"], &env).await;
    assert_eq!(missing.exit, Exit::Failure);
    assert!(
        missing.stderr.contains("rh local needs memory.sqlite.path"),
        "{}",
        missing.stderr
    );
    let memory = home.path().join(".rivethub/memory-dir");
    std::fs::create_dir_all(&memory).expect("directory");
    config(
        &home,
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n",
            memory.display()
        ),
    );
    let migrate = run(&["local", "--no-lan"], &env).await;
    assert_eq!(migrate.exit, Exit::Failure);
    assert!(!home.path().join(".rivethub/rivethub.pid").exists());
}

#[tokio::test]
async fn port_zero_is_rejected() {
    let home = home();
    let env = environment(&home);
    let report = run(&["local", "--port", "0"], &env).await;
    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report
            .stderr
            .contains("rh local --port must be between 1 and 65535"),
        "{}",
        report.stderr
    );
    assert!(!home.path().join(".rivethub").exists());
}

#[tokio::test]
async fn an_existing_file_keeps_its_bytes_and_flags_apply_in_memory() {
    let home = home();
    let env = environment(&home);
    let database = home.path().join(".rivethub/memory.sqlite");
    config(
        &home,
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\ngateway:\n  host: 0.0.0.0\n  port: 5174\n",
            database.display()
        ),
    );
    let path = home.path().join(".rivethub/config.yaml");
    let before = std::fs::read(&path).expect("config");
    let cli = Cli::parse_args(["rh", "local", "--port", "4242", "--no-lan"].map(Into::into))
        .expect("parse");
    let dispatch = cli::dispatch(cli, &env, &tokio_util::sync::CancellationToken::new()).await;
    let Dispatch::Local(prepared) = dispatch else {
        panic!("local did not prepare");
    };
    assert_eq!(std::fs::read(&path).expect("config"), before);
    assert_eq!(
        prepared.config.gateway.as_ref().map(|section| section.port),
        Some(4242)
    );
    assert_eq!(
        prepared
            .config
            .gateway
            .as_ref()
            .and_then(|section| section.host.as_deref()),
        Some("127.0.0.1")
    );
    prepared.release().expect("release");
}

fn without_tls(home: &tempfile::TempDir) -> config::Environment {
    let shared = home.path().join("shared");
    config::Environment::from_vars([
        ("HOME".as_ref(), home.path().as_os_str()),
        ("RIVETHUB_SHARED_DIR".as_ref(), shared.as_os_str()),
    ])
}

#[tokio::test]
async fn a_first_run_without_tls_serves_loopback_only() {
    let home = home();
    let env = without_tls(&home);
    let cli = Cli::parse_args(["rh", "local"].map(Into::into)).expect("parse");
    let dispatch = cli::dispatch(cli, &env, &tokio_util::sync::CancellationToken::new()).await;
    let Dispatch::Local(prepared) = dispatch else {
        panic!("local did not prepare");
    };
    let text = std::fs::read_to_string(home.path().join(".rivethub/config.yaml")).expect("config");
    assert!(text.contains("host: 127.0.0.1"), "{text}");
    assert!(text.contains("port: 5174"), "{text}");
    prepared.release().expect("release");
}

#[tokio::test]
async fn a_lan_bind_without_tls_names_the_ways_out() {
    let home = home();
    let env = without_tls(&home);
    let database = home.path().join(".rivethub/memory.sqlite");
    std::fs::create_dir_all(home.path().join(".rivethub")).expect("state directory");
    std::fs::write(
        home.path().join(".rivethub/config.yaml"),
        format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\ngateway:\n  host: 0.0.0.0\n  port: 5174\n",
            database.display()
        ),
    )
    .expect("config");
    let report = run(&["local"], &env).await;
    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report.stderr.contains(
            "binds the gateway to 0.0.0.0, which needs TLS. Run `rh local --no-lan` to serve on 127.0.0.1 only"
        ),
        "{}",
        report.stderr
    );
    assert!(!home.path().join(".rivethub/rivethub.pid").exists());
    assert!(!database.exists());
}

async fn first_run(
    env: &config::Environment,
    args: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let home = env.home().ok_or("no HOME")?.to_owned();
    let cli =
        Cli::parse_args(args.iter().map(|arg| (*arg).into())).map_err(|report| report.stderr)?;
    let dispatch = cli::dispatch(cli, env, &tokio_util::sync::CancellationToken::new()).await;
    let Dispatch::Local(prepared) = dispatch else {
        return Err("local did not prepare".into());
    };
    let disabled = prepared.config.cloud_disabled();
    prepared.release()?;
    if !disabled {
        return Err("the prepared config keeps the cloud on".into());
    }
    Ok(std::fs::read_to_string(
        std::path::Path::new(&home).join(".rivethub/config.yaml"),
    )?)
}

#[tokio::test]
async fn a_cloud_off_first_run_writes_the_policy_into_the_new_config() {
    let flagged = home();
    let text = first_run(&without_tls(&flagged), &["rh", "local", "--no-cloud"])
        .await
        .expect("first run");
    assert!(text.contains("cloud:\n  disabled: true\n"), "{text}");
    let variable = home();
    let env = without_tls(&variable).with_override("RIVETHUB_CLOUD", "off");
    let text = first_run(&env, &["rh", "local"]).await.expect("first run");
    assert!(text.contains("cloud:\n  disabled: true\n"), "{text}");
}

#[tokio::test]
async fn a_cloud_off_switch_refuses_a_config_without_the_policy() {
    let home = home();
    let database = home.path().join(".rivethub/memory.sqlite");
    let without = format!(
        "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\ngateway:\n  host: 127.0.0.1\n  port: 9\n",
        database.display()
    );
    config(&home, &without);
    let path = home.path().join(".rivethub/config.yaml");
    let before = std::fs::read_to_string(&path).expect("config");
    let switched = environment(&home).with_override("RIVETHUB_CLOUD", "off");
    for (args, env) in [
        (vec!["local", "--no-cloud"], environment(&home)),
        (vec!["local", "--ensure", "--no-cloud"], environment(&home)),
        (vec!["local"], switched.clone()),
        (vec!["local", "--ensure"], switched),
    ] {
        let report = run(&args, &env).await;
        assert_eq!(report.exit, Exit::Failure, "{args:?}: {}", report.stdout);
        assert!(
            report.stderr.contains("add `cloud: {disabled: true}` to"),
            "{args:?}: {}",
            report.stderr
        );
        assert_eq!(std::fs::read_to_string(&path).expect("config"), before);
        assert!(!home.path().join(".rivethub/rivethub.pid").exists());
    }
    let unknown = run(
        &["local"],
        &environment(&home).with_override("RIVETHUB_CLOUD", "maybe"),
    )
    .await;
    assert_eq!(unknown.exit, Exit::Failure, "{}", unknown.stdout);
    assert!(
        unknown.stderr.contains("RIVETHUB_CLOUD"),
        "{}",
        unknown.stderr
    );

    config(&home, &format!("{without}cloud:\n  disabled: true\n"));
    let cli = Cli::parse_args(["rh", "local", "--no-cloud"].map(Into::into)).expect("parse");
    let dispatch = cli::dispatch(
        cli,
        &environment(&home),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;
    let Dispatch::Local(prepared) = dispatch else {
        panic!("a config with the policy did not prepare");
    };
    prepared.release().expect("release");
}
