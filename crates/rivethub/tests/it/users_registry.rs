use std::time::Duration;

use rivethub::composition::Composition;
use wire::gateway::users::{UserToken, UsersRegistry};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const REGISTRY: &str = include_str!("../../../wire/tests/golden/gateway/users-tenancy.json");

#[tokio::test]
async fn validation_refuses_missing_invalid_and_ambiguous_registries() -> TestResult {
    let home = tempfile::tempdir()?;
    let path = home.path().join("users.json");
    let config = config::Config {
        gateway: Some(config::model::GatewaySection {
            users_file: Some(path.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut duplicate: UsersRegistry = serde_json::from_str(REGISTRY)?;
    let digest = "49e2e40e591e61357758299c8cee170fb9fa7da160ec8acf110a4a409d905aaf".to_owned();
    for user in duplicate.users.values_mut() {
        user.tokens = Some(vec![UserToken::Digest(digest.clone())]);
    }
    let ambiguous = serde_json::to_string(&duplicate)?;
    for content in [
        None,
        Some(include_str!(
            "../../../wire/tests/golden/gateway/users-invalid.json"
        )),
        Some(r#"{"ownerUserId":"owner","unmappedIsOwner":"false","users":{}}"#),
        Some(ambiguous.as_str()),
        Some(REGISTRY),
    ] {
        if let Some(content) = content {
            tokio::fs::write(&path, content).await?;
        }
        let validated = Composition::validate_users(&config, &config::Environment::default()).await;
        assert_eq!(
            validated.is_ok(),
            content == Some(REGISTRY),
            "{validated:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_sqlite_node_serves_one_person_and_refuses_a_users_file_for_more() -> TestResult {
    let home = tempfile::tempdir()?;
    let state = home.path().join(".rivethub");
    tokio::fs::create_dir(&state).await?;
    let users_path = state.join("users.json");
    tokio::fs::write(
        state.join("config.yaml"),
        format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\ngateway:\n  host: 127.0.0.1\n  port: 5976\n  users_file: {}\n",
            state.join("memory.sqlite").display(),
            users_path.display()
        ),
    )
    .await?;
    let digest = |n: u8| format!("{:064x}", u64::from(n));
    for (users, accepted, says) in [
        (
            format!(
                r#"{{"ownerUserId":"alice","unmappedIsOwner":false,"users":{{"alice":{{"tokens":["{}"]}}}}}}"#,
                digest(1)
            ),
            true,
            "",
        ),
        (
            format!(
                r#"{{"ownerUserId":"alice","unmappedIsOwner":false,"users":{{"alice":{{"tokens":["{}"]}},"bob":{{"tokens":["{}"]}}}}}}"#,
                digest(1),
                digest(2)
            ),
            false,
            "bob has a token or a device",
        ),
        (
            r#"{"ownerUserId":"alice","unmappedIsOwner":false,"users":{},"spaces":{"home":{"kind":"household","members":["alice","bob"]}}}"#.to_owned(),
            false,
            "bob is a member of space home",
        ),
    ] {
        tokio::fs::write(&users_path, &users).await?;
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_rh"))
                .args(["gateway", "--check"])
                .env_clear()
                .env("HOME", home.path())
                .env("TOKIO_WORKER_THREADS", "2")
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.success(), accepted, "{users}: {stderr}");
        assert!(stderr.contains(says), "{stderr}");
        if !accepted {
            assert!(stderr.contains("serves one person"), "{stderr}");
        }
    }
    Ok(())
}
