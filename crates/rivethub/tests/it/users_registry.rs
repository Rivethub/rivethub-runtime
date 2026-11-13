use std::time::Duration;

use rivethub::composition::Composition;
use wire::gateway::users::{UserToken, UsersRegistry};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const REGISTRY: &str = include_str!("../../../wire/tests/golden/gateway/users-tenancy.json");

#[tokio::test]
async fn recorded_registry_is_the_composition_boundary() -> TestResult {
    let home = tempfile::tempdir()?;
    let path = home.path().join("users.json");
    tokio::fs::write(&path, REGISTRY).await?;
    let config = config::Config {
        gateway: Some(config::model::GatewaySection {
            users_file: Some(path),
            ..Default::default()
        }),
        ..Default::default()
    };
    Composition::validate_users(&config, &config::Environment::default()).await?;
    let users: UsersRegistry = serde_json::from_str(REGISTRY)?;
    assert_eq!(users.owner_user_id, "owner");
    assert!(!users.unmapped_is_owner);
    let alice = users.users.get("alice").ok_or("missing fixture user")?;
    assert_eq!(alice.devices, Some(vec!["device:test".into()]));
    assert_eq!(alice.persona.as_deref(), Some("alice"));
    Ok(())
}

#[tokio::test]
async fn check_refuses_explicit_missing_invalid_and_ambiguous_registries() -> TestResult {
    let home = tempfile::tempdir()?;
    let state = home.path().join(".rivethub");
    tokio::fs::create_dir(&state).await?;
    let users_path = state.join("users.json");
    tokio::fs::write(state.join("config.yaml"), format!(
        "runtime:\n  workspace: {}\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    model: m\ngateway:\n  users_file: {}\n",
        home.path().display(), users_path.display()
    )).await?;
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
            tokio::fs::write(&users_path, content).await?;
        }
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
        assert_eq!(
            output.status.success(),
            content == Some(REGISTRY),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[tokio::test]
async fn registry_environment_precedence_and_deprecated_alias_fail_closed() -> TestResult {
    let home = tempfile::tempdir()?;
    let path = home.path().join("users.json");
    tokio::fs::write(&path, REGISTRY).await?;
    let missing = home.path().join("missing.json");
    let config = config::Config::default();
    for variable in ["RIVETHUB_USERS_FILE", "RIVETOS_USERS_FILE"] {
        let env =
            config::Environment::from_vars([(std::ffi::OsStr::new(variable), missing.as_os_str())]);
        assert!(Composition::validate_users(&config, &env).await.is_err());
    }
    let env = config::Environment::from_vars([
        (
            std::ffi::OsStr::new("RIVETHUB_USERS_FILE"),
            path.as_os_str(),
        ),
        (
            std::ffi::OsStr::new("RIVETOS_USERS_FILE"),
            missing.as_os_str(),
        ),
    ]);
    Composition::validate_users(&config, &env).await?;
    Ok(())
}

#[tokio::test]
async fn local_identity_uses_the_wire_registry_default() -> TestResult {
    let users = UsersRegistry::default();
    assert_eq!(users.owner_user_id, wire::gateway::users::DEFAULT_OWNER_ID);
    assert_eq!(users.owner_user_id, "owner");
    assert!(!users.unmapped_is_owner);
    Composition::validate_users(&config::Config::default(), &config::Environment::default())
        .await?;
    assert!(
        serde_json::from_str::<UsersRegistry>(r#"{"unmappedIsOwner":false,"users":{}}"#).is_err()
    );
    Ok(())
}
