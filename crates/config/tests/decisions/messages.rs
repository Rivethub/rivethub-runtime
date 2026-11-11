use super::{Config, HEX_KEY, config_yaml, env_from, expect_validation_error};

#[test]
fn inventory_messages_that_the_recordings_do_not_reach() -> Result<(), Box<dyn std::error::Error>> {
    let cases: [(&str, &str, &str); 8] = [
        (
            "runtime:\n  workspace: w\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    model: m\ngateway:\n  host: \"   \"\n",
            "\"gateway.host\" must be a non-empty string (bind address, e.g. 127.0.0.1 or 0.0.0.0)",
            "blank gateway.host",
        ),
        (
            "runtime:\n  workspace: w\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    model: m\ngateway:\n  port: 0\n",
            "\"gateway.port\" must be an integer between 1 and 65535",
            "gateway.port 0",
        ),
        (
            "runtime:\n  workspace: w\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    model: m\ngateway:\n  allowed_origins:\n    - gateway.example.com\n    - \"\"\n",
            "\"gateway.allowed_origins\" must be a list of non-empty strings",
            "blank allowed_origins entry",
        ),
        (
            "runtime:\n  workspace: w\n  default_agent: main\n  turn_timeout: 0\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    model: m\n",
            "\"runtime.turn_timeout\" must be a positive number (seconds)",
            "turn_timeout 0",
        ),
        (
            "memory:\n  sqlite:\n    path: \"\"\n",
            "\"memory.sqlite.path\" must be a non-empty file path",
            "blank sqlite path",
        ),
        (
            "memory:\n  capture:\n    redaction:\n      patterns:\n        - \"\"\n",
            "Each pattern must be a non-empty string (JS regex source)",
            "empty pattern",
        ),
        (
            "memory:\n  capture:\n    redaction:\n      patterns:\n        - \"(a+)+b\"\n",
            concat!(
                "Pattern looks ReDoS-prone (nested quantifiers); rewrite without nested +/",
                "* groups"
            ),
            "ReDoS pattern",
        ),
        (
            "memory:\n  capture:\n    redaction:\n      patterns:\n        - \"[unclosed\"\n",
            "Invalid regex: ",
            "invalid regex",
        ),
    ];
    for (yaml, message, label) in cases {
        let formatted =
            expect_validation_error(yaml).map_err(|error| format!("{label}: {error}"))?;
        assert!(
            formatted.contains(message),
            "{label}: expected \"{message}\" in:\n{formatted}"
        );
    }
    Ok(())
}

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
