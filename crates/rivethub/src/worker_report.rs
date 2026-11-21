use std::sync::Arc;

use config::provider::ProviderBody;
use config::{Config, Environment, ModelChoice, ModelsError};
use gateway::HubEmbedder;
use kernel::queue::WorkerRole;
use kernel::tidy::{ModelLabel, REPORT_FRESH, REPORT_INTERVAL, WorkerReportStore};
use tokio_util::sync::CancellationToken;

fn trimmed(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}

fn endpoint(config: &Config, id: &str) -> Option<String> {
    match &config.providers.as_ref()?.get(id)?.body {
        ProviderBody::Openai(body) => body.base_url.as_deref().map(trimmed),
        ProviderBody::Cli(_) | ProviderBody::Unknown(_) | ProviderBody::Removed(_) => None,
    }
}

fn chat(config: &Config, choice: &ModelChoice) -> ModelLabel {
    ModelLabel {
        provider: endpoint(config, &choice.provider).or_else(|| Some(choice.provider.clone())),
        model: config.model_named(choice),
    }
}

pub fn roles(
    config: &Config,
    env: &Environment,
    selected: &[WorkerRole],
) -> Result<Vec<(WorkerRole, ModelLabel)>, ModelsError> {
    let models = config.memory_models(env)?;
    let embedder = models.embedder.choice.as_ref().map(|choice| ModelLabel {
        provider: choice
            .provider
            .as_deref()
            .and_then(|id| endpoint(config, id))
            .or_else(|| choice.endpoint.as_deref().map(trimmed)),
        model: config.embedder_model_named(choice),
    });
    Ok(selected
        .iter()
        .map(|&role| {
            let label = match role {
                WorkerRole::Summarizer => chat(config, &models.summarizer.choice),
                WorkerRole::Categorizer => chat(config, &models.categorizer.choice),
                WorkerRole::WikiBuilder | WorkerRole::EntityBuilder => {
                    chat(config, &models.wiki_writer.choice)
                }
                WorkerRole::Embedder => match &embedder {
                    Some(label) => label.clone(),
                    None => chat(
                        config,
                        &ModelChoice {
                            model: None,
                            ..models.default.clone()
                        },
                    ),
                },
            };
            (role, label)
        })
        .collect())
}

#[must_use]
pub fn hub_embedder(config: &Config, env: &Environment) -> HubEmbedder {
    let label = config
        .memory_embed_endpoint(env)
        .map(|endpoint| ModelLabel {
            provider: Some(trimmed(&endpoint)),
            model: config.memory_embed_model(env),
        });
    let index_key = label
        .as_ref()
        .and_then(|_| crate::models::summary_index_key(config, env))
        .map(hex::encode);
    HubEmbedder { label, index_key }
}

pub(crate) async fn warn_on_embedder_mismatch(
    store: Arc<dyn WorkerReportStore>,
    hub: HubEmbedder,
    cancel: CancellationToken,
) {
    if hub.index_key.is_none() {
        return;
    }
    let reports = tokio::select! {
        biased;
        () = cancel.cancelled() => return,
        reports = async {
            tokio::time::sleep(REPORT_INTERVAL).await;
            store.newest_first(REPORT_FRESH).await
        } => reports,
    };
    match reports {
        Ok(reports) => {
            let mismatch = reports
                .iter()
                .filter(|report| report.role == WorkerRole::Embedder)
                .find_map(|report| gateway::embedder_mismatch(&hub, report));
            if let Some(mismatch) = mismatch {
                tracing::warn!("{mismatch}");
            }
        }
        Err(error) => tracing::warn!("worker reports were not read: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use config::{Config, Environment};
    use kernel::queue::WorkerRole;
    use kernel::tidy::ModelLabel;

    const WORKER: &str = "runtime:
  workspace: /home/user/work
  default_agent: summarizer
agents:
  summarizer:
    provider: local
providers:
  local:
    type: openai-compatible
    base_url: http://gpu.example:8003/v1/
    model: flash
  cli:
    type: claude-cli
    model: opus
memory:
  backend: postgres
  postgres:
    connection_string: postgres://user@db.example/memory
    embed_endpoint: http://embed.example:9402/
    embed_model: qwen3-embedding-0.6b
  models:
    default:
      provider: local
    categorizer:
      provider: cli
";

    fn label(provider: &str, model: &str) -> ModelLabel {
        ModelLabel {
            provider: Some(provider.to_owned()),
            model: Some(model.to_owned()),
        }
    }

    #[test]
    fn a_worker_names_the_endpoint_and_model_each_role_calls()
    -> Result<(), Box<dyn std::error::Error>> {
        let env = Environment::default();
        let config = Config::from_str(&env, WORKER)?.config;
        let flash = label("http://gpu.example:8003/v1", "flash");
        let embedder = label("http://embed.example:9402", "qwen3-embedding-0.6b");
        assert_eq!(
            super::roles(
                &config,
                &env,
                &[
                    WorkerRole::Summarizer,
                    WorkerRole::Categorizer,
                    WorkerRole::WikiBuilder,
                    WorkerRole::EntityBuilder,
                    WorkerRole::Embedder,
                ]
            )?,
            [
                (WorkerRole::Summarizer, flash.clone()),
                (WorkerRole::Categorizer, label("cli", "opus")),
                (WorkerRole::WikiBuilder, flash.clone()),
                (WorkerRole::EntityBuilder, flash),
                (WorkerRole::Embedder, embedder.clone()),
            ]
        );
        let hub = super::hub_embedder(&config, &env);
        assert_eq!(hub.label, Some(embedder));
        assert_eq!(hub.index_key.map(|key| key.len()), Some(64));
        Ok(())
    }

    #[test]
    fn a_worker_without_an_embedder_names_the_model_its_provider_embeds_with()
    -> Result<(), Box<dyn std::error::Error>> {
        let env = Environment::default();
        let yaml = WORKER
            .replace("    embed_endpoint: http://embed.example:9402/\n", "")
            .replace("    embed_model: qwen3-embedding-0.6b\n", "")
            .replace(
                "    default:\n      provider: local\n",
                "    default:\n      provider: local\n      model: chat-override\n",
            );
        let config = Config::from_str(&env, &yaml)?.config;
        assert_eq!(
            super::roles(
                &config,
                &env,
                &[WorkerRole::Summarizer, WorkerRole::Embedder]
            )?,
            [
                (
                    WorkerRole::Summarizer,
                    label("http://gpu.example:8003/v1", "chat-override")
                ),
                (
                    WorkerRole::Embedder,
                    label("http://gpu.example:8003/v1", "flash")
                ),
            ]
        );
        assert_eq!(
            super::hub_embedder(&config, &env),
            gateway::HubEmbedder::default()
        );
        Ok(())
    }
}
