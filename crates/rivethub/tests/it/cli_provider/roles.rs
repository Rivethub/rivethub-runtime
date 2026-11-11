use std::ffi::OsStr;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::routing::{get, post};
use config::provider::ProviderType;
use kernel::harness::SessionEvent;
use rivethub::models::ModelFactory;
use rivethub::worker::role_bindings;
use tokio_util::sync::CancellationToken;
use wire::stream::openai::{EmbeddingRequest, EmbeddingResponse, EmbeddingRow, Model, ModelList};

use super::{TestResult, configured, harness, recorded, turn};

async fn served(id: &'static str) -> std::io::Result<testkit::Stub> {
    let listing = move || async move {
        Json(ModelList {
            object: "list".to_owned(),
            data: vec![Model {
                id: id.to_owned(),
                object: "model".to_owned(),
                created: 0,
                owned_by: "test".to_owned(),
                root: None,
                parent: None,
                max_model_len: None,
                meta: None,
                permission: None,
            }],
        })
    };
    testkit::stub_server(Router::new().route("/v1/models", get(listing))).await
}

fn configured_at(
    home: &std::path::Path,
    text: &str,
) -> TestResult<(config::Config, config::Environment)> {
    let env = config::Environment::from_vars([(OsStr::new("HOME"), home.as_os_str())]);
    let path = home.join("config.yaml");
    std::fs::write(&path, text)?;
    let loaded = config::Config::load(
        &env,
        &config::ConfigLocation::Current(path),
        config::Profile::Node,
    )?;
    Ok((loaded.config, env))
}

#[tokio::test]
async fn every_role_gets_its_own_provider_and_model() -> TestResult {
    let big = served("served-big").await?;
    let small = served("served-small").await?;
    let embed = testkit::stub_server(Router::new().route(
        "/v1/embeddings",
        post(|Json(request): Json<EmbeddingRequest>| async move {
            assert_eq!(request.model, "bge-test");
            Json(EmbeddingResponse {
                data: vec![EmbeddingRow {
                    index: 0,
                    embedding: vec![0.5, 0.25],
                }],
            })
        }),
    ))
    .await?;
    let home = tempfile::tempdir()?;
    let text = format!(
        "runtime:\n  workspace: {home}\n  default_agent: main\nagents:\n  main:\n    provider: big\n    model: agent-model\nproviders:\n  big:\n    type: vllm\n    base_url: http://{big}\n    model: big-model\n  small:\n    type: vllm\n    base_url: http://{small}\n    model: small-model\nmesh:\n  enabled: false\n  node_name: test-node\nmemory:\n  sqlite:\n    path: ':memory:'\n  models:\n    default:\n      provider: big\n      model: big-model\n    summarizer:\n      provider: small\n      model: small-summarizer\n    wiki_writer:\n      provider: big\n      model: big-writer\n    embedder:\n      endpoint: http://{embed}\n      model: bge-test\n",
        home = home.path().display(),
        big = big.addr,
        small = small.addr,
        embed = embed.addr,
    );
    let (config, env) = configured_at(home.path(), &text)?;
    let cancel = CancellationToken::new();
    let factory = ModelFactory::from_config(&config, &env, &cancel).await?;
    let bindings = role_bindings(&config, &factory, &cancel)?;

    assert_eq!(bindings.summarizer.model, "small-summarizer");
    assert_eq!(
        bindings.summarizer.provider.list_models().await?,
        vec!["served-small".to_owned()]
    );
    assert_eq!(bindings.categorizer.model, "big-model");
    assert_eq!(
        bindings.categorizer.provider.list_models().await?,
        vec!["served-big".to_owned()]
    );
    assert_eq!(bindings.wiki_writer.model, "big-writer");
    assert_eq!(
        bindings.wiki_writer.provider.list_models().await?,
        vec!["served-big".to_owned()]
    );
    assert!(bindings.embeddings);
    let vectors = bindings.embedder.embed(vec!["text".to_owned()]).await?;
    assert_eq!(vectors.len(), 1);
    assert_eq!(
        vectors.first().map(|vector| vector.vector.clone()),
        Some(vec![0.5, 0.25])
    );
    Ok(())
}

#[tokio::test]
async fn absent_models_give_every_role_the_default_agent() -> TestResult {
    let big = served("served-big").await?;
    let home = tempfile::tempdir()?;
    let text = format!(
        "runtime:\n  workspace: {home}\n  default_agent: main\nagents:\n  main:\n    provider: big\n    model: agent-model\nproviders:\n  big:\n    type: vllm\n    base_url: http://{big}\n    model: big-model\nmesh:\n  enabled: false\n  node_name: test-node\nmemory:\n  sqlite:\n    path: ':memory:'\n",
        home = home.path().display(),
        big = big.addr,
    );
    let (config, env) = configured_at(home.path(), &text)?;
    let cancel = CancellationToken::new();
    let factory = ModelFactory::from_config(&config, &env, &cancel).await?;
    let bindings = role_bindings(&config, &factory, &cancel)?;
    for model in [
        &bindings.summarizer.model,
        &bindings.categorizer.model,
        &bindings.wiki_writer.model,
    ] {
        assert_eq!(model, "agent-model");
    }
    assert_eq!(
        bindings.wiki_writer.provider.list_models().await?,
        vec!["served-big".to_owned()]
    );
    assert!(bindings.embeddings);
    Ok(())
}

#[tokio::test]
async fn a_role_effort_reaches_the_harness_and_others_keep_the_provider_effort() -> TestResult {
    let home = tempfile::tempdir()?;
    let models = "memory:\n  sqlite:\n    path: ':memory:'\n    embed_endpoint: http://127.0.0.1:9\n    embed_model: bge-test\n  models:\n    default:\n      provider: p\n      model: default-model\n    wiki_writer:\n      provider: p\n      model: writer-model\n      effort: low\n";
    let (config, _) = configured(home.path(), ProviderType::ClaudeCli, models)?;
    let (events, _): (Vec<SessionEvent>, String) = recorded()?;
    let harness = Arc::new(harness::Harness::new(events));
    let factory = ModelFactory::default().with_harness("p".into(), harness.clone());
    let cancel = CancellationToken::new();
    let bindings = role_bindings(&config, &factory, &cancel)?;
    assert_eq!(bindings.wiki_writer.model, "writer-model");
    assert_eq!(bindings.summarizer.model, "default-model");
    bindings
        .wiki_writer
        .provider
        .chat(turn("writer-model"))
        .await?;
    bindings
        .summarizer
        .provider
        .chat(turn("default-model"))
        .await?;
    let seen: Vec<_> = harness
        .requests
        .lock()
        .await
        .iter()
        .map(|request| (request.model.clone(), request.effort))
        .collect();
    assert_eq!(
        seen,
        vec![
            (
                Some("writer-model".to_owned()),
                Some(wire::stream::ThinkingLevel::Low)
            ),
            (
                Some("default-model".to_owned()),
                Some(wire::stream::ThinkingLevel::High)
            ),
        ]
    );
    Ok(())
}
