use std::net::SocketAddr;
use std::path::Path;

use wire::gateway::memory::MemorySearchResponse;
use wire::stream::openai::{EmbeddingRequest, EmbeddingResponse, EmbeddingRow};

use super::{TestResult, provider_at, start, stop, write_state};

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
    write_state(
        home.path(),
        &provider_at(home.path(), "http://127.0.0.1:1", &body),
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

#[tokio::test]
async fn a_custom_sqlite_query_instruction_prefixes_the_query() -> TestResult {
    let (_, calls) = local_vector_search(|database, embed| {
        format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    embed_endpoint: http://{embed}\n    embed_model: embedding-model\n    embed_query_instruction: \"query: \"\n",
            database.display()
        )
    })
    .await?;
    let inputs: Vec<Vec<String>> = calls.into_iter().map(|call| call.input).collect();
    assert_eq!(inputs, vec![vec![format!("query: {QUERY}")]]);
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
