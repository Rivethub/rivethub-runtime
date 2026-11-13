use std::net::SocketAddr;
use std::path::Path;

use wire::gateway::memory::MemorySearchResponse;
use wire::stream::openai::{EmbeddingRequest, EmbeddingResponse, EmbeddingRow};

use super::{TestResult, provider_base, start, stop, write_config};

const QUERY: &str = "when does acmekit deploy";

#[derive(Debug)]
struct Call {
    authorization: Option<String>,
    input: Vec<String>,
}

async fn local_vector_search(
    extra: impl FnOnce(&Path, SocketAddr) -> String,
) -> TestResult<(MemorySearchResponse, Vec<Call>)> {
    let (calls_tx, mut calls_rx) = tokio::sync::mpsc::unbounded_channel::<Call>();
    let router = axum::Router::new().route(
        "/v1/embeddings",
        axum::routing::post(
            move |headers: axum::http::HeaderMap,
                  axum::Json(request): axum::Json<EmbeddingRequest>| {
                let calls = calls_tx.clone();
                async move {
                    let rows = request
                        .input
                        .iter()
                        .enumerate()
                        .map(|(index, _)| EmbeddingRow {
                            index,
                            embedding: vec![0.6, 0.8],
                        })
                        .collect();
                    let authorization = headers
                        .get("authorization")
                        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
                    drop(calls.send(Call {
                        authorization,
                        input: request.input,
                    }));
                    axum::Json(EmbeddingResponse { data: rows })
                }
            },
        ),
    );
    let server = testkit::stub_server(router).await?;
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let database = home.path().join(".rivethub").join("memory.sqlite");
    if let Some(parent) = database.parent() {
        std::fs::create_dir_all(parent)?;
    }
    migrate::sqlite(&database)?;
    let body = format!(
        "{}gateway:\n  host: 127.0.0.1\n  port: {port}\n",
        extra(&database, server.addr)
    );
    write_config(
        home.path(),
        &provider_base(home.path(), "http://127.0.0.1:1", &body),
    )?;
    let started = start(
        home.path(),
        &["local", "--no-lan"],
        &["gateway listening on "],
    )
    .await?;
    let searched = reqwest::Client::builder()
        .no_proxy()
        .default_headers(crate::owner::home_headers(home.path())?)
        .build()?
        .get(format!(
            "http://127.0.0.1:{port}/api/memory/search?q={}&mode=vector",
            QUERY.replace(' ', "%20")
        ))
        .send()
        .await;
    stop(started).await?;
    let response = searched?
        .error_for_status()?
        .json::<MemorySearchResponse>()
        .await?;
    let mut calls = Vec::new();
    while let Ok(call) = calls_rx.try_recv() {
        calls.push(call);
    }
    Ok((response, calls))
}

async fn default_inputs(model: &str) -> TestResult<Vec<Vec<String>>> {
    let (response, calls) = local_vector_search(|database, embed| {
        format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    embed_endpoint: http://{embed}\n    embed_model: {model}\n",
            database.display()
        )
    })
    .await?;
    assert_eq!(response.degraded, None, "{response:?}");
    Ok(calls.into_iter().map(|call| call.input).collect())
}

#[tokio::test]
async fn local_vector_search_embeds_the_bare_query_for_a_non_qwen_model() -> TestResult {
    assert_eq!(
        default_inputs("embedding-model").await?,
        vec![vec![QUERY.to_owned()]]
    );
    Ok(())
}

#[tokio::test]
async fn local_vector_search_prefixes_the_qwen_instruction_for_a_qwen_model() -> TestResult {
    assert_eq!(
        default_inputs("Qwen/Qwen3-Embedding-0.6B").await?,
        vec![vec![format!(
            "{}{QUERY}",
            kernel::memory_core::DEFAULT_EMBED_QUERY_INSTRUCTION
        )]]
    );
    Ok(())
}

async fn instructed_inputs(instruction: &str) -> TestResult<Vec<Vec<String>>> {
    let (_, calls) = local_vector_search(|database, embed| {
        format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    embed_endpoint: http://{embed}\n    embed_model: embedding-model\n    embed_query_instruction: \"{instruction}\"\n",
            database.display()
        )
    })
    .await?;
    Ok(calls.into_iter().map(|call| call.input).collect())
}

#[tokio::test]
async fn an_empty_sqlite_query_instruction_embeds_the_bare_query() -> TestResult {
    assert_eq!(instructed_inputs("").await?, vec![vec![QUERY.to_owned()]]);
    Ok(())
}

#[tokio::test]
async fn a_custom_sqlite_query_instruction_prefixes_the_query() -> TestResult {
    assert_eq!(
        instructed_inputs("query: ").await?,
        vec![vec![format!("query: {QUERY}")]]
    );
    Ok(())
}

#[tokio::test]
async fn local_vector_search_embeds_through_the_named_provider_with_its_minted_bearer() -> TestResult
{
    let (response, calls) = local_vector_search(|database, embed| {
        format!(
            "  embedder:\n    type: vllm\n    base_url: http://{embed}\n    model: served-model\n    token_command:\n      - /bin/echo\n      - minted-embed-token\nmemory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    embed_provider: embedder\n    embed_model: embedding-model\n",
            database.display()
        )
    })
    .await?;
    assert_eq!(response.degraded, None, "{response:?}");
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls.iter().all(|call| {
            call.authorization.as_deref() == Some("Bearer minted-embed-token")
                && call.input.iter().any(|input| input.contains(QUERY))
        }),
        "{calls:?}"
    );
    Ok(())
}
