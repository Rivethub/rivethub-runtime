use super::{Config, HEX_KEY, config_yaml, env_from, expect_validation_error};

#[test]
fn claude_cli_provider_messages_that_the_recordings_do_not_reach()
-> Result<(), Box<dyn std::error::Error>> {
    let formatted = expect_validation_error(&config_yaml(
        "  main-provider:\n    type: claude-cli\n  acme:\n    type: claude-cli\n    permission_prompts: \"yes\"\n",
        "",
    ))?;
    assert!(
        formatted.contains(
            "permission_prompts must be \"ui\" or \"none\" (omit the key to leave spawns unchanged)"
        ),
        "unexpected message: {formatted}"
    );

    let formatted = expect_validation_error(&config_yaml(
        "  main-provider:\n    type: claude-cli\n  acme:\n    type: claude-cli\n    permission_timeout_ms: 700000\n",
        "",
    ))?;
    assert!(
        formatted.contains(
            "permission_timeout_ms must be a positive integer of at most 600000 (default 60000)"
        ),
        "unexpected message: {formatted}"
    );
    Ok(())
}

#[test]
fn pipeline_validates_before_env_resolution() -> Result<(), Box<dyn std::error::Error>> {
    let yaml = config_yaml(
        "  main-provider:\n    model: acme-model\n    api_key: \"${ACME_API_KEY}\"\n",
        "",
    );
    let loaded = Config::from_str(&env_from(&[("ACME_API_KEY", HEX_KEY)]), &yaml)?;
    assert!(
        loaded
            .warnings
            .iter()
            .all(|finding| !finding.message.contains("hardcoded")),
        "an unresolved variable must not look hardcoded"
    );
    Ok(())
}
