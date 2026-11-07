use config::{Config, Environment};
use wire::gateway::memory_models::ModelRole;

use super::ComposeError;

pub(super) fn attach(
    gateway: gateway::Gateway,
    config: &Config,
    env: &Environment,
    factory: &crate::models::ModelFactory,
) -> Result<gateway::Gateway, ComposeError> {
    let assignments = if config.memory_models_available() {
        assignments(config, env)?
    } else {
        Vec::new()
    };
    Ok(gateway
        .with_provider_health(factory.health())
        .with_model_assignments(assignments))
}

fn assignments(
    config: &Config,
    env: &Environment,
) -> Result<Vec<gateway::ModelAssignment>, ComposeError> {
    let models = config
        .memory_models(env)
        .map_err(|error| ComposeError::Invalid(error.to_string()))?;
    let chat = |role, choice: &config::ModelChoice, inherits_default| gateway::ModelAssignment {
        role,
        provider: Some(choice.provider.clone()),
        model: config.model_named(choice),
        inherits_default,
    };
    let embedder = match &models.embedder.choice {
        Some(choice) => gateway::ModelAssignment {
            role: ModelRole::Embedder,
            provider: choice.provider.clone().or_else(|| choice.endpoint.clone()),
            model: config.embedder_model_named(choice),
            inherits_default: models.embedder.inherits_default,
        },
        None => chat(ModelRole::Embedder, &models.default, true),
    };
    Ok(vec![
        chat(ModelRole::Default, &models.default, false),
        chat(
            ModelRole::Summarizer,
            &models.summarizer.choice,
            models.summarizer.inherits_default,
        ),
        chat(
            ModelRole::Categorizer,
            &models.categorizer.choice,
            models.categorizer.inherits_default,
        ),
        chat(
            ModelRole::WikiWriter,
            &models.wiki_writer.choice,
            models.wiki_writer.inherits_default,
        ),
        embedder,
    ])
}
