#[path = "common/mod.rs"]
#[expect(
    dead_code,
    reason = "the harness module is included whole by every test binary and this one uses a subset"
)]
mod common;

use std::future::Future;
use std::time::Duration;

use common::FixedStatusServer;
use common::demand;
use common::fixed_status_server;
use kernel::model::Embedding;
use kernel::model::ModelProvider;
use providers::BuildError;
use providers::embed::EmbedConfig;
use providers::embed::EmbedError;
use providers::embed::Embedder;
use providers::error::StatusError;
use reqwest::StatusCode;
use wire::stream::openai::EmbeddingRequest;
use wire::stream::openai::EmbeddingResponse;
use wire::stream::openai::EmbeddingRow;

const EIGHT_DIMS: &str = "{\"object\":\"list\",\"data\":[{\"object\":\"embedding\",\"embedding\":[0.544317,-0.861921,-0.400774,0.1,-0.2,0.3,0.7,-0.9],\"index\":0}],\"model\":\"bge-m3\",\"usage\":{\"prompt_tokens\":4,\"total_tokens\":4}}";

const RECORDED_VECTOR: [f32; 8] = [0.544_317, -0.861_921, -0.400_774, 0.1, -0.2, 0.3, 0.7, -0.9];

fn base_config(server: &FixedStatusServer) -> EmbedConfig {
    EmbedConfig {
        endpoint: format!("http://{}", server.addr),
        api_prefix: None,
        model: "bge-m3".to_owned(),
        api_key: None,
        expected_dims: None,
        timeout_ms: Some(5_000),
        query_instruction: None,
    }
}

fn embedder(config: EmbedConfig) -> Embedder {
    demand(Embedder::build(config), "the embedder builds")
}

async fn within<F: Future>(future: F) -> F::Output {
    demand(
        tokio::time::timeout(Duration::from_secs(10), future).await,
        "the embed call finishes in time",
    )
}

fn rows(rows: Vec<EmbeddingRow>) -> String {
    demand(
        serde_json::to_string(&EmbeddingResponse { data: rows }),
        "the response serializes",
    )
}

fn row(index: usize, embedding: Vec<f32>) -> EmbeddingRow {
    EmbeddingRow { embedding, index }
}

async fn embed_against(
    status: u16,
    body: String,
    texts: &[&str],
) -> Result<Vec<Embedding>, EmbedError> {
    let server = fixed_status_server(status, body).await;
    let embedder = embedder(base_config(&server));
    within(embedder.embed(texts.iter().map(|text| (*text).to_owned()).collect())).await
}

#[tokio::test]
async fn a_well_formed_response_is_one_embedding_per_input() {
    let server = fixed_status_server(200, EIGHT_DIMS.to_owned()).await;
    let embedder = embedder(EmbedConfig {
        api_key: Some("embed-key".to_owned()),
        expected_dims: Some(8),
        query_instruction: Some("Instruct: retrieve\nQuery: ".to_owned()),
        ..base_config(&server)
    });
    let out = within(embedder.embed(vec!["hello world".to_owned()]))
        .await
        .expect("the embed succeeds");
    assert_eq!(
        out,
        vec![Embedding {
            vector: RECORDED_VECTOR.to_vec()
        }]
    );
    assert_eq!(
        embedder.query_instruction(),
        Some("Instruct: retrieve\nQuery: ")
    );
    let call = server
        .recorder
        .lock()
        .first()
        .cloned()
        .expect("the embed call is captured");
    assert_eq!(call.method, "POST");
    assert_eq!(call.target, "/v1/embeddings");
    assert_eq!(call.authorization.as_deref(), Some("Bearer embed-key"));
    let sent: EmbeddingRequest = serde_json::from_str(&call.body).expect("the sent body parses");
    assert_eq!(
        sent,
        EmbeddingRequest {
            model: "bge-m3".to_owned(),
            input: vec!["hello world".to_owned()],
        }
    );
}

#[tokio::test]
async fn rows_are_placed_by_index_not_by_position() {
    let body = rows(vec![
        row(2, vec![2.0]),
        row(0, vec![0.0]),
        row(1, vec![1.0]),
    ]);
    let out = embed_against(200, body, &["a", "b", "c"])
        .await
        .expect("out-of-order rows embed");
    let expected = [0.0_f32, 1.0, 2.0]
        .into_iter()
        .map(|value| Embedding {
            vector: vec![value],
        })
        .collect::<Vec<_>>();
    assert_eq!(out, expected);
}

#[tokio::test]
async fn a_row_without_an_index_is_malformed() {
    let error = embed_against(200, "{\"data\":[{\"embedding\":[0.5]}]}".to_owned(), &["a"])
        .await
        .expect_err("a missing index fails");
    assert!(
        matches!(error, EmbedError::Malformed { .. }),
        "a missing index is a malformed response: {error:?}"
    );
}

#[tokio::test]
async fn an_index_that_is_not_a_whole_number_is_malformed() {
    for index in ["\"0\"", "0.5", "-1", "null"] {
        let body = format!("{{\"data\":[{{\"embedding\":[0.5],\"index\":{index}}}]}}");
        let error = embed_against(200, body, &["a"])
            .await
            .expect_err("a non-integer index fails");
        assert!(
            matches!(error, EmbedError::Malformed { .. }),
            "index {index} is malformed: {error:?}"
        );
    }
}

#[tokio::test]
async fn a_duplicate_index_is_an_error() {
    let body = rows(vec![row(0, vec![0.0]), row(0, vec![1.0])]);
    let error = embed_against(200, body, &["a", "b"])
        .await
        .expect_err("a duplicate index fails");
    assert!(
        matches!(error, EmbedError::DuplicateIndex { index: 0 }),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_index_past_the_inputs_is_an_error() {
    let body = rows(vec![row(0, vec![0.0]), row(2, vec![1.0])]);
    let error = embed_against(200, body, &["a", "b"])
        .await
        .expect_err("an out-of-range index fails");
    assert!(
        matches!(error, EmbedError::IndexOutOfRange { index: 2, count: 2 }),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_input_without_a_row_is_missing() {
    let body = rows(vec![row(1, vec![1.0])]);
    let error = embed_against(200, body, &["a", "b"])
        .await
        .expect_err("a missing row fails");
    assert!(
        matches!(error, EmbedError::Missing { index: 0 }),
        "{error:?}"
    );
}

#[tokio::test]
async fn wrong_dimensions_are_an_error() {
    let server = fixed_status_server(200, EIGHT_DIMS.to_owned()).await;
    let embedder = embedder(EmbedConfig {
        expected_dims: Some(1024),
        ..base_config(&server)
    });
    let error = within(embedder.embed(vec!["hello".to_owned()]))
        .await
        .expect_err("the dimension mismatch fails");
    assert!(
        matches!(
            error,
            EmbedError::Dimensions {
                index: 0,
                got: 8,
                expected: 1024
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_non_2xx_status_carries_the_status_and_body() {
    let body = "{\"error\":{\"message\":\"model not loaded\"}}";
    let error = embed_against(503, body.to_owned(), &["a"])
        .await
        .expect_err("the status fails the embed");
    let EmbedError::Status(StatusError::Body {
        status,
        body: received,
        ..
    }) = error
    else {
        common::halt(&format!("a non-2xx status is a status error: {error:?}"));
    };
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(received, body);
}

#[tokio::test]
async fn a_malformed_body_is_an_error() {
    for body in [
        "not json",
        "{\"embeddings\":[[0.5]]}",
        "{\"data\":[{\"embedding\":\"0.5\",\"index\":0}]}",
        "{\"data\":[{\"embedding\":[\"0.5\"],\"index\":0}]}",
        "{\"data\":\"rows\"}",
    ] {
        let error = embed_against(200, body.to_owned(), &["a"])
            .await
            .expect_err("a malformed body fails");
        assert!(
            matches!(error, EmbedError::Malformed { .. }),
            "{body} is malformed: {error:?}"
        );
    }
}

#[tokio::test]
async fn embed_honours_the_api_prefix_and_sends_no_auth_without_a_key() {
    let server = fixed_status_server(200, EIGHT_DIMS.to_owned()).await;
    let embedder = embedder(EmbedConfig {
        endpoint: format!("http://{}/", server.addr),
        api_prefix: Some("openai/v1".to_owned()),
        ..base_config(&server)
    });
    within(embedder.embed(vec!["hello".to_owned()]))
        .await
        .expect("the embed succeeds");
    let call = server
        .recorder
        .lock()
        .first()
        .cloned()
        .expect("the embed call is captured");
    assert_eq!(call.target, "/openai/v1/embeddings");
    assert_eq!(call.authorization, None);
}

#[tokio::test]
async fn the_embedder_refuses_an_incomplete_config() {
    let server = fixed_status_server(200, EIGHT_DIMS.to_owned()).await;
    let no_endpoint = Embedder::build(EmbedConfig {
        endpoint: String::new(),
        ..base_config(&server)
    });
    assert!(matches!(no_endpoint, Err(BuildError::MissingEmbedEndpoint)));
    let no_model = Embedder::build(EmbedConfig {
        model: String::new(),
        ..base_config(&server)
    });
    assert!(matches!(no_model, Err(BuildError::MissingEmbedModel)));
    let wrong_scheme = Embedder::build(EmbedConfig {
        endpoint: "ftp://example.com".to_owned(),
        ..base_config(&server)
    });
    assert!(matches!(wrong_scheme, Err(BuildError::BaseUrlScheme)));
}

#[tokio::test]
async fn the_chat_provider_embeds_with_its_prefix_and_bearer() {
    let server = fixed_status_server(200, EIGHT_DIMS.to_owned()).await;
    let provider = config::provider::OpenaiProvider {
        model: Some("test-model".to_owned()),
        base_url: Some(format!("http://{}", server.addr)),
        api_prefix: Some("openai/v1".to_owned()),
        api_key: Some("chat-key".to_owned()),
        ..config::provider::OpenaiProvider::default()
    };
    let built = providers::OpenAiCompatible::build(config::provider::ProviderType::Vllm, &provider)
        .expect("the provider builds");
    let out = within(built.embed(vec!["hello".to_owned()]))
        .await
        .expect("the chat provider embeds");
    assert_eq!(out.len(), 1);
    assert_eq!(
        out.first().map(|embedding| embedding.vector.as_slice()),
        Some(RECORDED_VECTOR.as_slice())
    );
    let call = server
        .recorder
        .lock()
        .first()
        .cloned()
        .expect("the embed call is captured");
    assert_eq!(call.method, "POST");
    assert_eq!(call.target, "/openai/v1/embeddings");
    assert_eq!(call.authorization.as_deref(), Some("Bearer chat-key"));
    let body: EmbeddingRequest = demand(serde_json::from_str(&call.body), "the embed body parses");
    assert_eq!(body.model, "test-model");
    assert_eq!(body.input, vec!["hello".to_owned()]);
}

#[tokio::test]
async fn the_chat_provider_embeds_without_auth_when_no_key_is_set() {
    let server = fixed_status_server(200, EIGHT_DIMS.to_owned()).await;
    let provider = config::provider::OpenaiProvider {
        model: Some("test-model".to_owned()),
        base_url: Some(format!("http://{}", server.addr)),
        ..config::provider::OpenaiProvider::default()
    };
    let built = providers::OpenAiCompatible::build(config::provider::ProviderType::Vllm, &provider)
        .expect("the provider builds");
    within(built.embed(vec!["hello".to_owned()]))
        .await
        .expect("the embed succeeds");
    let call = server
        .recorder
        .lock()
        .first()
        .cloned()
        .expect("the embed call is captured");
    assert_eq!(call.authorization, None);
    assert_eq!(call.target, "/v1/embeddings");
}

#[tokio::test]
async fn an_embed_provider_embeds_and_refuses_chat() {
    let server = fixed_status_server(200, EIGHT_DIMS.to_owned()).await;
    let provider =
        providers::EmbedProvider::new(embedder(base_config(&server)), "bge-m3".to_owned());
    let out = within(provider.embed(vec!["hello world".to_owned()]))
        .await
        .expect("the embed succeeds");
    assert_eq!(
        out,
        vec![Embedding {
            vector: RECORDED_VECTOR.to_vec()
        }]
    );
    assert_eq!(
        provider.list_models().await.expect("the model list"),
        vec!["bge-m3".to_owned()]
    );
    let request = kernel::model::ModelRequest {
        options: kernel::model::ModelOptions::default(),
        model: "bge-m3".to_owned(),
        messages: Vec::new(),
        tools: Vec::new(),
        thinking: None,
        max_tokens: None,
    };
    let refused = provider.chat(request).await.expect_err("chat is refused");
    assert!(
        refused.to_string().contains("only serves embeddings"),
        "{refused}"
    );
}
