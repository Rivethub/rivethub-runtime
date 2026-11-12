use config::provider::ProviderBody;
use config::{Config, ConfigError, Environment, ProviderType};

fn document(kind: ProviderType, fields: &str) -> String {
    format!(
        "runtime:\n  workspace: /home/user/work\n  default_agent: main\nagents:\n  main:\n    provider: harness\nproviders:\n  harness:\n    type: {kind}\n{fields}"
    )
}

#[test]
fn all_cli_entries_preserve_headless_session_configuration() {
    for kind in [
        ProviderType::ClaudeCli,
        ProviderType::CodexCli,
        ProviderType::GrokCli,
        ProviderType::HermesCli,
        ProviderType::KimiCode,
        ProviderType::OpencodeCli,
    ] {
        let loaded = Config::from_str(&Environment::default(), &document(kind,
            "    model: test-model\n    cwd: /home/user/work\n    effort: high\n    timeout_ms: 9000\n    system_prompt: Be concise\n"
        )).expect("CLI config loads");
        let providers = loaded.config.providers.expect("provider entries");
        let slot = providers.get("harness").expect("harness provider");
        assert_eq!(slot.kind, Some(kind));
        let ProviderBody::Cli(entry) = &slot.body else {
            panic!("CLI provider shape")
        };
        assert_eq!(entry.model.as_deref(), Some("test-model"));
        assert_eq!(
            entry.cwd.as_deref(),
            Some(std::path::Path::new("/home/user/work"))
        );
        assert_eq!(entry.effort, Some(config::Effort::High));
        assert_eq!(entry.timeout_ms, Some(9000));
        assert_eq!(entry.system_prompt.as_deref(), Some("Be concise"));
    }
}

#[test]
fn absent_options_remain_absent_and_invalid_timeouts_are_rejected() {
    let loaded = Config::from_str(
        &Environment::default(),
        &document(ProviderType::CodexCli, ""),
    )
    .expect("optional config");
    let providers = loaded.config.providers.expect("providers");
    let ProviderBody::Cli(entry) = &providers.get("harness").expect("harness provider").body else {
        panic!("CLI entry")
    };
    assert!(entry.model.is_none());
    assert!(entry.cwd.is_none());
    assert!(entry.timeout_ms.is_none());
    for timeout in ["0", "-1", "1.5", "wrong"] {
        let error = Config::from_str(
            &Environment::default(),
            &document(
                ProviderType::CodexCli,
                &format!("    timeout_ms: {timeout}\n"),
            ),
        )
        .expect_err("invalid timeout");
        match error {
            ConfigError::Type { path, .. } => assert_eq!(path, "providers.harness.timeout_ms"),
            ConfigError::Validation { result } => {
                let message = result.to_string();
                assert!(message.contains("providers.harness.timeout_ms"));
                if timeout == "0" {
                    assert!(message.contains("timeout_ms must be a positive number"));
                } else {
                    assert!(message.contains("has the wrong type"));
                }
            }
            other @ (ConfigError::Io { .. }
            | ConfigError::Yaml(_)
            | ConfigError::Home
            | ConfigError::StateHome
            | ConfigError::NodeName
            | ConfigError::Substitution(_)
            | ConfigError::EnvironmentText { .. }
            | ConfigError::CloudSwitch(_)) => {
                panic!("unexpected error {other}")
            }
        }
    }
}

#[test]
fn discovery_timeout_is_its_own_positive_setting() {
    let loaded = Config::from_str(
        &Environment::default(),
        &document(
            ProviderType::HermesCli,
            "    timeout_ms: 9000\n    discovery_timeout_ms: 45000\n",
        ),
    )
    .expect("discovery timeout loads");
    let providers = loaded.config.providers.expect("providers");
    let ProviderBody::Cli(entry) = &providers.get("harness").expect("harness provider").body else {
        panic!("CLI entry")
    };
    assert_eq!(entry.discovery_timeout_ms, Some(45_000));
    assert_eq!(entry.timeout_ms, Some(9000));
    let error = Config::from_str(
        &Environment::default(),
        &document(ProviderType::HermesCli, "    discovery_timeout_ms: 0\n"),
    )
    .expect_err("zero discovery timeout");
    let ConfigError::Validation { result } = error else {
        panic!("validation error expected: {error}")
    };
    let message = result.to_string();
    assert!(message.contains("providers.harness.discovery_timeout_ms"));
    assert!(
        message.contains("Provider \"harness\" discovery_timeout_ms must be a positive number")
    );
}
