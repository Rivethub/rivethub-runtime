use kernel::wiki::entity_match::embed_line;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::PreparedEntity;
use uuid::Uuid;

use super::super::EntityBuilderEnv;
use super::super::embed::embed_text;

use super::bind::fact_line;
use super::bind::prepared_facts;

pub(in crate::entity_builder) async fn store_vector(
    env: &EntityBuilderEnv<'_>,
    id: Uuid,
    vector: Option<&[f32]>,
    view: &EntityView,
) {
    let generated = if vector.is_some_and(|item| item.len() == 1024) {
        None
    } else {
        let line = embed_line(&view.name, &view.aliases, &fact_line(view));
        embed_text(&env.settings.embed_url, &env.settings.embed_model, &line).await
    };
    let stored = vector
        .filter(|item| item.len() == 1024)
        .or(generated.as_deref());
    let Some(stored) = stored else {
        return;
    };
    if let Err(error) = env.store.remember_embedding(id, stored, env.now).await {
        tracing::warn!(%error, %id, "entity embedding was not stored");
    }
}

pub(in crate::entity_builder) async fn refresh_embedding(
    env: &EntityBuilderEnv<'_>,
    view: &EntityView,
    summary: &str,
) {
    let summary = summary.trim();
    if summary.is_empty() {
        return;
    }
    let line = embed_line(&view.name, &view.aliases, summary);
    let Some(vector) = embed_text(&env.settings.embed_url, &env.settings.embed_model, &line).await
    else {
        return;
    };
    if let Err(error) = env
        .store
        .remember_embedding(view.id, &vector, env.now)
        .await
    {
        tracing::warn!(%error, id = %view.id, "entity embedding was not refreshed");
    }
}

pub(super) async fn embed_observation(
    env: &EntityBuilderEnv<'_>,
    prepared: &PreparedEntity,
) -> Option<Vec<f32>> {
    let line = embed_line(&prepared.name, &prepared.aliases, &prepared_facts(prepared));
    embed_text(&env.settings.embed_url, &env.settings.embed_model, &line).await
}
