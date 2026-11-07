use std::path::PathBuf;

use config::{Config, ConfigError, Environment};

fn env_from(pairs: &[(&str, &str)]) -> Environment {
    Environment::from_vars(
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string())),
    )
}

fn config_yaml(provider_entries: &str, sections: &str) -> String {
    let providers = if provider_entries.is_empty() {
        "  main-provider:\n    model: acme-model\n".to_owned()
    } else {
        provider_entries.to_owned()
    };
    format!(
        "runtime:\n  workspace: /home/user/work\n  default_agent: main\nagents:\n  main:\n    provider: main-provider\nproviders:\n{providers}{sections}"
    )
}

fn load(text: &str) -> Result<Config, ConfigError> {
    let loaded = Config::from_str(&Environment::default(), text)?;
    Ok(loaded.config)
}

fn validation_error(text: &str) -> Result<String, String> {
    match Config::from_str(&Environment::default(), text) {
        Err(ConfigError::Validation { result }) => Ok(result.to_string()),
        Err(other) => Err(format!("{other}")),
        Ok(_) => Err("config loaded".to_owned()),
    }
}

fn expect_validation_error(text: &str) -> Result<String, Box<dyn std::error::Error>> {
    validation_error(text)
        .map_err(|error| format!("expected a validation error, got: {error}").into())
}

fn expect_loaded(text: &str) -> Result<Config, Box<dyn std::error::Error>> {
    load(text).map_err(|error| format!("expected the config to load, got: {error}").into())
}

const HEX_KEY: &str = "0123456789abcdef00112233445566778899aabbccddeeff0011223344556677";

#[test]
fn decision_16_new_env_names_win_and_legacy_names_warn() {
    let env = env_from(&[("RIVETOS_SHARED_DIR", "/srv/legacy")]);
    assert_eq!(config::shared_dir(&env), PathBuf::from("/srv/legacy"));
    assert!(
        env.legacy_notice("RIVETHUB_SHARED_DIR", "RIVETOS_SHARED_DIR")
            .is_some()
    );

    let env = env_from(&[
        ("RIVETHUB_SHARED_DIR", "/srv/new"),
        ("RIVETOS_SHARED_DIR", "/srv/legacy"),
    ]);
    assert_eq!(config::shared_dir(&env), PathBuf::from("/srv/new"));
    assert!(
        env.legacy_notice("RIVETHUB_SHARED_DIR", "RIVETOS_SHARED_DIR")
            .is_none()
    );
}

#[test]
fn decision_16_probe_prefers_rivethub_home() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("rivethub-config-probe-{}", std::process::id()));
    let hub = root.join(".rivethub");
    let legacy = root.join(".rivetos");
    std::fs::create_dir_all(&hub)?;
    std::fs::create_dir_all(&legacy)?;
    std::fs::write(hub.join("config.yaml"), "runtime: {}\n")?;
    std::fs::write(legacy.join("config.yaml"), "runtime: {}\n")?;

    let env = env_from(&[("HOME", root.to_str().ok_or("temp path")?)]);
    let location = config::resolve_config_path(&env)?.ok_or("no config candidate found")?;
    assert!(location.path().ends_with(".rivethub/config.yaml"));
    assert!(!location.is_legacy());

    std::fs::remove_file(hub.join("config.yaml"))?;
    let location = config::resolve_config_path(&env)?.ok_or("no legacy config candidate found")?;
    assert!(location.path().ends_with(".rivetos/config.yaml"));
    assert!(location.is_legacy());

    std::fs::remove_file(legacy.join("config.yaml"))?;
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn decision_9_memory_backend_resolves() -> Result<(), Box<dyn std::error::Error>> {
    let config = expect_loaded(&config_yaml(
        "",
        "memory:\n  postgres:\n    connection_string: postgres://example.com/db\n",
    ))?;
    assert!(!config.sqlite_selected());
    assert!(!config.pglite_import_pending());

    let config = expect_loaded(&config_yaml(
        "",
        "memory:\n  postgres:\n    embedded:\n      data_dir: /home/user/.rivetos/pg\n",
    ))?;
    assert!(config.sqlite_selected());
    assert!(config.pglite_import_pending());

    let config = expect_loaded(&config_yaml("", ""))?;
    assert!(!config.sqlite_selected());

    let config = expect_loaded(&config_yaml("", "memory:\n  backend: postgres\n"))?;
    assert!(!config.sqlite_selected());

    let config = expect_loaded(&config_yaml(
        "",
        "memory:\n  backend: sqlite\n  postgres:\n    connection_string: postgres://example.com/db\n",
    ))?;
    assert!(config.sqlite_selected());

    let formatted = expect_validation_error(&config_yaml("", "memory:\n  backend: pglite\n"))?;
    assert!(
        formatted.contains("unknown variant `pglite`"),
        "unexpected message: {formatted}"
    );
    Ok(())
}

#[test]
fn decision_10_removed_tool_gates_and_plugin_entries_are_ignored_with_warnings() {
    let env = env_from(&[
        ("RIVETHUB_MCP_ENABLE_SHELL", "1"),
        ("RIVETOS_MCP_ENABLE_FILE", "1"),
    ]);
    let warnings = env.deprecation_warnings();
    assert_eq!(warnings.len(), 2);
    assert!(warnings.first().is_some_and(|warning| {
        warning.contains("RIVETHUB_MCP_ENABLE_SHELL") && warning.contains("ignored")
    }));
    assert!(warnings.get(1).is_some_and(|warning| {
        warning.contains("RIVETOS_MCP_ENABLE_FILE") && warning.contains("ignored")
    }));
}

#[test]
fn decision_10_plugin_list_is_accepted_and_ignored() -> Result<(), Box<dyn std::error::Error>> {
    let loaded = Config::from_str(
        &Environment::default(),
        &config_yaml(
            "",
            "plugins:\n  - plugins/tools/shell\n  - \"@acme/rivetos-plugin\"\n",
        ),
    )?;
    let plugin_warnings: Vec<&String> = loaded
        .warnings
        .iter()
        .map(|finding| &finding.message)
        .filter(|message| message.contains("plugins/tools/shell"))
        .collect();
    assert!(
        plugin_warnings.is_empty(),
        "the section warning does not name entries: {plugin_warnings:?}"
    );
    assert!(
        loaded
            .warnings
            .iter()
            .any(|finding| finding.message == config::PLUGINS_IGNORED),
        "expected the accepted-and-ignored warning"
    );
    assert!(
        loaded
            .notices
            .iter()
            .any(|notice| notice.contains("plugins/tools/shell")),
        "expected a per-entry load-time notice"
    );
    Ok(())
}

#[test]
fn decision_11_provider_type_selects_behaviour() -> Result<(), Box<dyn std::error::Error>> {
    let banned = [
        ("anthropic", "claude-opus", "claude-code"),
        ("google", "gemini-2", "gemini"),
        ("gemini", "gemini-2", "gemini"),
        ("xai", "grok-4", "grok-build"),
    ];
    for (kind, model, harness) in banned {
        let yaml = config_yaml(
            &format!(
                "  main-provider:\n    model: acme-model\n  acme:\n    type: {kind}\n    model: {model}\n"
            ),
            "",
        );
        let formatted = expect_validation_error(&yaml)?;
        assert!(
            formatted.contains(&format!("type \"{kind}\" is not built")),
            "banned type {kind}: {formatted}"
        );
        assert!(
            formatted.contains(harness),
            "harness {harness}: {formatted}"
        );
    }

    let kept = [
        "  acme:\n    type: openai-compatible\n    model: acme-model\n    base_url: http://example.com/v1\n",
        "  acme:\n    type: codex-cli\n",
    ];
    for entries in kept {
        expect_loaded(&config_yaml(
            &format!("  main-provider:\n    model: acme-model\n{entries}"),
            "",
        ))?;
    }

    let formatted = expect_validation_error(&config_yaml(
        "  main-provider:\n    model: acme-model\n  acme:\n    type: vllm\n    model: acme-model\n",
        "",
    ))?;
    assert!(
        formatted.contains("Provider \"acme\" requires \"base_url\""),
        "unexpected message: {formatted}"
    );

    let result = Config::from_str(
        &Environment::default(),
        &config_yaml(
            "  main-provider:\n    model: acme-model\n  acme:\n    type: unknown-kind\n    model: m\n",
            "",
        ),
    )?;
    assert!(
        result.warnings.iter().any(|finding| finding.message
            == "Unknown provider type \"unknown-kind\" — make sure a registrar handles it"),
        "expected the unknown-type warning"
    );
    Ok(())
}

#[test]
fn decision_11_removed_provider_types_name_their_alternative()
-> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        (
            "ollama",
            "reach Ollama through a harness, or use type: openai-compatible with base_url http://<host>:11434/v1",
        ),
        (
            "llama-server",
            "use type: openai-compatible with base_url <server>/v1",
        ),
        ("openai-compat", "spelled openai-compatible"),
        ("cli", "no CLI provider of that type; use a harness"),
        ("pi-cli", "no CLI provider of that type; use a harness"),
        ("qwen-code", "no CLI provider of that type; use a harness"),
    ];
    for (kind, alternative) in cases {
        let yaml = config_yaml(
            &format!("  main-provider:\n    model: acme-model\n  acme:\n    type: {kind}\n"),
            "",
        );
        let formatted = expect_validation_error(&yaml)?;
        let expected = format!("Provider \"acme\" type \"{kind}\" is not built — {alternative}.");
        assert!(
            formatted.contains(&expected),
            "removed type {kind}: expected \"{expected}\" in:\n{formatted}"
        );
    }
    Ok(())
}

#[test]
fn removed_provider_keys_warn_and_the_provider_still_boots()
-> Result<(), Box<dyn std::error::Error>> {
    let loaded = Config::from_str(
        &Environment::default(),
        &config_yaml(
            "  main-provider:\n    model: acme-model\n  acme:\n    type: vllm\n    model: acme-model\n    base_url: http://example.com/v1\n    temperature: 0.7\n    top_p: 0.95\n    models:\n      - acme-model\n    models_ttl_ms: 60000\n    token_command:\n      - /home/user/bin/token\n    keep_alive: 5m\n",
            "",
        ),
    )?;
    let warnings: Vec<&String> = loaded
        .warnings
        .iter()
        .map(|finding| &finding.path)
        .collect();
    for key in [
        "providers.acme.temperature",
        "providers.acme.top_p",
        "providers.acme.models",
        "providers.acme.models_ttl_ms",
        "providers.acme.keep_alive",
    ] {
        assert!(
            warnings.contains(&&key.to_owned()),
            "expected a warning at {key}: {warnings:?}"
        );
    }
    assert!(
        loaded.warnings.iter().all(|finding| {
            !finding.message.contains("token_command wins")
                && !finding.message.contains("typical range")
        }),
        "this fixture sets token_command without api_key, so it must not warn that token_command wins: {warnings:?}"
    );
    for finding in &loaded.warnings {
        if finding.path.starts_with("providers.acme.") {
            let Some(key) = finding.path.rsplit('.').next() else {
                continue;
            };
            assert_eq!(
                finding.message,
                format!("{key} is set on the model server, not in RivetHub"),
                "unexpected message: {:?}",
                finding
            );
        }
    }
    Ok(())
}

#[test]
fn token_command_is_kept_and_wins_over_a_static_key() -> Result<(), Box<dyn std::error::Error>> {
    let loaded = Config::from_str(
        &Environment::default(),
        &config_yaml(
            "  main-provider:\n    model: acme-model\n  acme:\n    type: vllm\n    model: acme-model\n    base_url: http://example.com/v1\n    api_key: static-key\n    token_command:\n      - /home/user/bin/token\n      - --mint\n    token_ttl_ms: 1000\n    token_command_timeout_ms: 250\n",
            "",
        ),
    )?;
    let wins = loaded
        .warnings
        .iter()
        .find(|finding| finding.path == "providers.acme.token_command")
        .ok_or("missing token_command wins warning")?;
    assert_eq!(
        wins.message,
        "Provider \"acme\" has both api_key and token_command — token_command wins"
    );
    let slot = loaded
        .config
        .providers
        .as_ref()
        .and_then(|map| map.get("acme"))
        .ok_or("acme is missing")?;
    let config::provider::ProviderBody::Openai(provider) = &slot.body else {
        return Err("acme is not an openai-family provider".into());
    };
    assert_eq!(
        provider.token_command.as_deref(),
        Some(["/home/user/bin/token".to_owned(), "--mint".to_owned()].as_slice())
    );
    assert_eq!(provider.token_ttl_ms, Some(1_000));
    assert_eq!(provider.token_command_timeout_ms, Some(250));

    let alone = Config::from_str(
        &Environment::default(),
        &config_yaml(
            "  main-provider:\n    model: acme-model\n  acme:\n    type: vllm\n    model: acme-model\n    base_url: http://example.com/v1\n    token_command:\n      - /home/user/bin/token\n",
            "",
        ),
    )?;
    assert!(
        alone
            .warnings
            .iter()
            .all(|finding| finding.path != "providers.acme.token_command"),
        "token_command without api_key is not a warning: {:?}",
        alone.warnings
    );

    let rejected = expect_validation_error(&config_yaml(
        "  main-provider:\n    model: acme-model\n  acme:\n    type: vllm\n    model: acme-model\n    base_url: http://example.com/v1\n    token_command: []\n    token_ttl_ms: 0\n    token_command_timeout_ms: 0\n",
        "",
    ))?;
    assert!(
        rejected.contains("token_command must be a non-empty argv array of strings"),
        "{rejected}"
    );
    assert!(
        rejected.contains("token_ttl_ms must be a positive number"),
        "{rejected}"
    );
    assert!(
        rejected.contains("token_command_timeout_ms must be a positive number"),
        "{rejected}"
    );
    Ok(())
}

#[test]
fn removed_embed_keys_warn_the_same_way() -> Result<(), Box<dyn std::error::Error>> {
    let postgres = Config::from_str(
        &Environment::default(),
        &config_yaml(
            "",
            "memory:\n  postgres:\n    connection_string: postgres://example.com/db\n    embed_token_command:\n      - /home/user/bin/token\n    embed_wire_shape: openai\n",
        ),
    )?;
    let sqlite = Config::from_str(
        &Environment::default(),
        &config_yaml(
            "",
            "memory:\n  sqlite:\n    path: ':memory:'\n    embed_token_command:\n      - /home/user/bin/token\n    embed_wire_shape: openai\n",
        ),
    )?;
    for loaded in [postgres, sqlite] {
        let paths: Vec<&String> = loaded
            .warnings
            .iter()
            .map(|finding| &finding.path)
            .collect();
        let section = if paths.contains(&&"memory.sqlite.embed_wire_shape".to_owned()) {
            "memory.sqlite"
        } else {
            "memory.postgres"
        };
        for key in ["embed_token_command", "embed_wire_shape"] {
            let path = format!("{section}.{key}");
            assert!(
                paths.contains(&&path),
                "expected a warning at {path}: {paths:?}"
            );
        }
        assert!(
            loaded.warnings.iter().all(|finding| finding.message
                == "embed_token_command is set on the model server, not in RivetHub"
                || finding.message
                    == "embed_wire_shape is set on the model server, not in RivetHub"
                || finding.path == "providers.main-provider"),
            "unexpected messages: {:?}",
            loaded.warnings
        );
    }
    Ok(())
}

#[test]
fn decision_12_chat_channel_config_is_accepted_and_ignored_with_warning()
-> Result<(), Box<dyn std::error::Error>> {
    let result = Config::from_str(
        &Environment::default(),
        &config_yaml(
            "",
            "channels:\n  discord:\n    bot_token: acme-bot-token-value\n",
        ),
    )?;
    assert!(
        result.warnings.iter().any(|finding| finding.message
            == "Unknown channel type \"discord\" — make sure a registrar handles it"),
        "expected the unknown-channel warning"
    );
    Ok(())
}

#[test]
fn decision_13_workflows_section_is_accepted_and_ignored() -> Result<(), Box<dyn std::error::Error>>
{
    let result = Config::from_str(
        &Environment::default(),
        &config_yaml(
            "",
            "workflows:\n  enabled: not-a-boolean\n  runs_dir: 5\n  defs_roots: \"x\"\n",
        ),
    )?;
    let (accepted, others): (Vec<_>, Vec<_>) = result
        .warnings
        .iter()
        .partition(|finding| finding.message == config::WORKFLOWS_IGNORED);
    assert_eq!(accepted.len(), 1, "expected exactly the accepted warning");
    assert!(
        others.iter().all(|finding| finding.path != "workflows"
            && !finding.path.starts_with("workflows.")
            && !finding.path.starts_with("workflows[")),
        "the workflows section must produce no findings beyond the accepted-and-ignored warning"
    );
    Ok(())
}

#[test]
fn decision_2_terminal_and_voice_variables_are_ignored_with_a_warning() {
    for (name, value) in [
        ("RIVETHUB_TERM_MUX", "tmux"),
        ("RIVETOS_TERM_MUX", "herdr"),
        ("RIVETHUB_UPLOAD_MAX_BYTES", "1"),
        ("RIVETHUB_VOICE_STT_URL", "http://127.0.0.1:1"),
    ] {
        let warnings = env_from(&[(name, value)]).deprecation_warnings();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.starts_with(&format!("{name} is no longer used"))),
            "{name}: {warnings:?}"
        );
    }
    assert_eq!(
        env_from(&[]).deprecation_warnings(),
        [] as [std::string::String; 0]
    );
}

#[test]
fn decision_15_redaction_is_a_real_setting() -> Result<(), Box<dyn std::error::Error>> {
    let config = expect_loaded(&config_yaml("", ""))?;
    assert!(
        !config.capture_redaction().enabled,
        "redaction defaults to off"
    );

    let config = expect_loaded(&config_yaml(
        "",
        "memory:\n  capture:\n    redaction:\n      enabled: true\n      patterns:\n        - \"acme-[0-9]+\"\n",
    ))?;
    let redaction = config.capture_redaction();
    assert!(redaction.enabled);
    assert!(redaction.builtins);
    assert_eq!(redaction.patterns, ["acme-[0-9]+"]);

    let result = Config::from_str(
        &Environment::default(),
        &config_yaml(
            "",
            "memory:\n  capture:\n    redaction:\n      enabled: true\n",
        ),
    )?;
    assert!(
        result
            .warnings
            .iter()
            .all(|finding| !finding.message.contains("not yet injected")),
        "decision 15 removes the not-injected warning"
    );
    Ok(())
}

#[test]
fn inventory_documented_defaults_appear_in_the_typed_config()
-> Result<(), Box<dyn std::error::Error>> {
    let config = expect_loaded(&config_yaml(
        "",
        "gateway:\n  host: 127.0.0.1\nmemory:\n  postgres:\n    embedded:\n      data_dir: /home/user/.rivetos/pg\n",
    ))?;
    let gateway = config.gateway.as_ref().ok_or("gateway section missing")?;
    assert_eq!(gateway.port, 5174, "gateway.port default");
    let postgres = config
        .memory
        .as_ref()
        .ok_or("memory section missing")?
        .postgres
        .as_ref()
        .ok_or("memory.postgres missing")?;
    let embedded = postgres.embedded.as_ref().ok_or("embedded missing")?;
    assert_eq!(embedded.port, 5433, "embedded.port default");
    assert_eq!(
        embedded.max_connections, 96,
        "embedded.max_connections default"
    );
    Ok(())
}

#[path = "decisions/messages.rs"]
mod messages;
