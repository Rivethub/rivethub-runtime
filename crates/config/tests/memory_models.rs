use config::{Config, ConfigError, Environment};

fn yaml(models: &str) -> String {
    format!(
        "runtime:\n  workspace: /home/user/work\n  default_agent: main\nagents:\n  main:\n    provider: chat\n    model: agent-model\nproviders:\n  chat:\n    type: claude-cli\n    model: cli-model\n  bare:\n    type: claude-cli\n  local:\n    type: vllm\n    base_url: http://models.example.com/v1\n    model: served-model\n  embedder:\n    type: vllm\n    base_url: http://embed.example.com/v1\n    model: embed-served\nmemory:\n  sqlite:\n    path: ':memory:'\n{models}"
    )
}

fn load(models: &str) -> Result<config::Loaded, ConfigError> {
    Config::from_str(&Environment::default(), &yaml(models))
}

fn rejected(models: &str) -> Result<String, String> {
    match load(models) {
        Err(ConfigError::Validation { result }) => Ok(result.to_string()),
        Err(other) => Err(other.to_string()),
        Ok(_) => Err(format!("expected a validation error for {models}")),
    }
}

#[test]
fn absent_models_derive_from_the_default_agent_without_a_notice()
-> Result<(), Box<dyn std::error::Error>> {
    let env = Environment::default();
    let loaded = load("")?;
    assert!(
        loaded
            .notices
            .iter()
            .all(|notice| !notice.contains("memory.models")),
        "{:?}",
        loaded.notices
    );
    let resolved = loaded.config.memory_models(&env)?;
    assert!(resolved.derived);
    assert_eq!(resolved.default.provider, "chat");
    assert_eq!(resolved.default.model.as_deref(), Some("agent-model"));
    assert!(resolved.summarizer.inherits_default);
    assert_eq!(resolved.wiki_writer.choice, resolved.default);
    assert_eq!(resolved.embedder.choice, None);
    assert!(resolved.embedder.inherits_default);
    Ok(())
}

#[test]
fn absent_models_derive_the_embedder_from_the_embed_keys() -> Result<(), Box<dyn std::error::Error>>
{
    let text = yaml("    embed_endpoint: http://embed.example.com/v1\n    embed_model: bge\n");
    let loaded = Config::from_str(&Environment::default(), &text)?;
    let embedder = loaded
        .config
        .memory_models(&Environment::default())?
        .embedder
        .choice
        .ok_or("the derived embedder")?;
    assert_eq!(
        embedder.endpoint.as_deref(),
        Some("http://embed.example.com/v1")
    );
    assert_eq!(embedder.model.as_deref(), Some("bge"));
    Ok(())
}

#[test]
fn explicit_models_assign_roles_and_inherit_the_rest() -> Result<(), Box<dyn std::error::Error>> {
    let loaded = load(
        "  models:\n    default:\n      provider: chat\n      model: big\n    summarizer:\n      provider: local\n      model: small\n    wiki_writer:\n      provider: chat\n      model: writer\n      effort: high\n    embedder:\n      provider: embedder\n",
    )?;
    assert!(
        loaded
            .notices
            .iter()
            .all(|notice| !notice.contains("memory.models")),
        "{:?}",
        loaded.notices
    );
    let resolved = loaded.config.memory_models(&Environment::default())?;
    assert!(!resolved.derived);
    assert_eq!(resolved.default.model.as_deref(), Some("big"));
    assert!(!resolved.embedder.inherits_default);
    assert!(!resolved.summarizer.inherits_default);
    assert_eq!(resolved.summarizer.choice.provider, "local");
    assert!(resolved.categorizer.inherits_default);
    assert_eq!(resolved.categorizer.choice, resolved.default);
    assert_eq!(
        resolved.wiki_writer.choice.effort,
        Some(config::Effort::High)
    );
    assert_eq!(
        resolved
            .embedder
            .choice
            .and_then(|embedder| embedder.provider),
        Some("embedder".to_owned())
    );
    Ok(())
}

#[test]
fn validation_names_each_bad_models_key() -> Result<(), String> {
    let cases = [
        (
            "  models:\n    default:\n      provider: nowhere\n      model: x\n",
            "Provider \"nowhere\" referenced by memory.models.default is not defined in [providers]",
        ),
        (
            "  models:\n    default:\n      provider: local\n      model: x\n    categorizer:\n      provider: absent\n      model: y\n",
            "memory.models.categorizer",
        ),
        (
            "  models:\n    default:\n      provider: chat\n    summarizer:\n      provider: bare\n",
            "provider \"bare\" has no model of its own",
        ),
        (
            "  models:\n    default:\n      provider: local\n    embedder:\n      provider: embedder\n      endpoint: http://embed.example.com/v1\n      model: m\n",
            "\"provider\" and \"endpoint\" cannot both be set",
        ),
        (
            "  models:\n    default:\n      provider: local\n    embedder:\n      model: m\n",
            "set \"provider\" or \"endpoint\" for the embedder",
        ),
        (
            "  models:\n    default:\n      provider: local\n    embedder:\n      endpoint: http://embed.example.com/v1\n",
            "\"memory.models.embedder.model\" is required with an endpoint",
        ),
        (
            "  models:\n    default:\n      provider: local\n    embedder:\n      provider: chat\n",
            "cannot serve embeddings",
        ),
        (
            "  models:\n    default:\n      provider: local\n    embedder:\n      endpoint: embed.example.com\n      model: m\n",
            "must be an http:// or https:// URL",
        ),
    ];
    for (models, expected) in cases {
        let report = rejected(models)?;
        if !report.contains(expected) {
            return Err(format!("{models}\nmissing {expected}\n{report}"));
        }
    }
    Ok(())
}

#[test]
fn effort_on_a_server_provider_and_unknown_keys_warn() -> Result<(), Box<dyn std::error::Error>> {
    let loaded = load(
        "  models:\n    default:\n      provider: local\n      effort: low\n      colour: red\n    nickname: x\n",
    )?;
    let paths: Vec<&str> = loaded
        .warnings
        .iter()
        .map(|finding| finding.path.as_str())
        .collect();
    assert!(paths.contains(&"memory.models.default.effort"), "{paths:?}");
    assert!(paths.contains(&"memory.models.default.colour"), "{paths:?}");
    assert!(paths.contains(&"memory.models.nickname"), "{paths:?}");
    Ok(())
}

#[test]
fn a_models_section_without_a_default_is_refused() -> Result<(), String> {
    let report = rejected("  models:\n    summarizer:\n      provider: local\n      model: x\n")?;
    assert!(report.contains("missing field `default`"), "{report}");
    Ok(())
}

#[test]
fn a_config_without_models_or_a_default_agent_has_no_assignments() -> Result<(), ConfigError> {
    let loaded = load("")?;
    assert!(loaded.config.memory_models_available());
    let bare = Config::default();
    assert!(!bare.memory_models_available());
    assert_eq!(
        bare.memory_models(&Environment::default()),
        Err(config::ModelsError::Runtime)
    );
    Ok(())
}
