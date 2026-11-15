use crate::common;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;

use crate::common::fixed_status_server;
use crate::common::scenario_server;
use config::provider::OpenaiProvider;
use config::provider::ProviderType;
use kernel::model::ModelProvider;
use kernel::model::ModelProviderError;
use kernel::model::ModelRequest;
use providers::OpenAiCompatible;
use testkit::Canned;
use wire::message::Message;
use wire::message::MessageContent;
use wire::message::Role;

const EMBEDDING: &str = "{\"object\":\"list\",\"data\":[{\"object\":\"embedding\",\"embedding\":[0.1],\"index\":0}],\"model\":\"test-model\",\"usage\":{\"prompt_tokens\":1,\"total_tokens\":1}}";

const MODEL_LIST: &str = "{\"object\":\"list\",\"data\":[{\"id\":\"test-model\",\"object\":\"model\",\"created\":0,\"owned_by\":\"vllm\"}]}";

const CHAT_SSE: &str = "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"test-model\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n";

struct Scratch {
    _directory: tempfile::TempDir,
    path: PathBuf,
}

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

fn scratch(label: &str) -> Scratch {
    let directory = match tempfile::tempdir() {
        Ok(directory) => directory,
        Err(error) => common::halt(&format!("a scratch directory: {error}")),
    };
    let path = directory.path().join(label);
    Scratch {
        _directory: directory,
        path,
    }
}

fn shell(script: &str) -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()]
}

fn dummy() -> SocketAddr {
    std::net::SocketAddr::from(([127, 0, 0, 1], 9))
}

fn provider(
    addr: SocketAddr,
    argv: Option<Vec<String>>,
    key: Option<&str>,
    ttl: Option<u64>,
    timeout: Option<u64>,
) -> OpenAiCompatible {
    let config = OpenaiProvider {
        model: Some("test-model".to_owned()),
        base_url: Some(format!("http://{addr}")),
        api_key: key.map(str::to_owned),
        token_command: argv,
        token_ttl_ms: ttl,
        token_command_timeout_ms: timeout,
        ..OpenaiProvider::default()
    };
    common::demand(
        OpenAiCompatible::build(ProviderType::Vllm, &config),
        "the provider builds",
    )
}

fn lines(path: &Path) -> usize {
    common::demand(std::fs::read_to_string(path), "the mint log reads")
        .lines()
        .count()
}

fn hi_request() -> ModelRequest {
    ModelRequest {
        options: kernel::model::ModelOptions::default(),
        model: "test-model".to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: MessageContent::Text("hi".to_owned()),
            reasoning: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        thinking: None,
        max_tokens: None,
    }
}

#[tokio::test]
async fn a_failing_token_command_is_an_error_without_the_secret() {
    let built = provider(
        dummy(),
        Some(shell("printf 'secret-token'; exit 3")),
        None,
        None,
        None,
    );
    let text = built
        .embed(vec!["q".to_owned()])
        .await
        .expect_err("the command fails")
        .to_string();
    assert!(text.contains("exited 3"), "{text}");
    assert!(!text.contains("secret-token"), "{text}");
}

#[tokio::test]
async fn token_stdout_over_64kib_is_an_error() {
    let built = provider(
        dummy(),
        Some(vec![
            "/usr/bin/head".to_owned(),
            "-c".to_owned(),
            "65537".to_owned(),
            "/dev/zero".to_owned(),
        ]),
        None,
        None,
        Some(5_000),
    );
    let text = built
        .embed(vec!["q".to_owned()])
        .await
        .expect_err("the cap trips")
        .to_string();
    assert!(text.contains("64KiB"), "{text}");
    assert!(text.len() < 200, "{text}");
}

#[tokio::test]
async fn a_slow_token_command_is_killed_at_the_timeout() {
    let pidfile = scratch("pid");
    let script = format!(
        "printf '%s\\n' $$ > '{}'; exec /bin/sleep 30",
        pidfile.display()
    );
    let built = provider(dummy(), Some(shell(&script)), None, None, Some(500));
    let started = std::time::Instant::now();
    let text = built
        .embed(vec!["hi".to_owned()])
        .await
        .expect_err("the command times out")
        .to_string();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the command ran for {:?}",
        started.elapsed()
    );
    assert!(text.contains("timed out"), "{text}");
    let pid = std::fs::read_to_string(&pidfile).expect("the pid file");
    let proc = PathBuf::from(format!("/proc/{}", pid.trim()));
    assert!(!proc.exists(), "pid {} is still alive", pid.trim());
}

#[tokio::test]
async fn two_embeds_share_one_mint_and_the_command_wins() {
    let server = fixed_status_server(200, EMBEDDING.to_owned()).await;
    let log = scratch("cache");
    let script = format!(
        "printf 'x\\n' >> '{}'; printf 'minted-token\\n'",
        log.display()
    );
    let built = provider(
        server.addr,
        Some(shell(&script)),
        Some("static-secret"),
        None,
        None,
    );
    built
        .embed(vec!["q".to_owned()])
        .await
        .expect("the first embed");
    built
        .embed(vec!["q".to_owned()])
        .await
        .expect("the cached embed");
    let calls = server.recorder.lock().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(lines(&log), 1);
    for call in &calls {
        assert_eq!(call.authorization.as_deref(), Some("Bearer minted-token"));
    }
}

#[tokio::test]
async fn a_static_key_401_is_not_reminted() {
    let server = scenario_server(vec![Canned {
        status: 401,
        body: "static-unauthorized".to_owned(),
    }])
    .await;
    let built = provider(server.addr, None, Some("static-key"), None, None);
    let error = built
        .embed(vec!["q".to_owned()])
        .await
        .expect_err("the server refuses");
    let ModelProviderError::Request { message, source } = error else {
        panic!("a status is a request error");
    };
    assert_eq!(message, "http 401");
    let rendered = source.to_string();
    assert!(rendered.contains("static-unauthorized"), "{rendered}");
    assert!(rendered.contains("model server returned"), "{rendered}");
    let calls = server.recorder.lock().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls.first().and_then(|call| call.authorization.as_deref()),
        Some("Bearer static-key")
    );
}

#[tokio::test]
async fn a_401_remints_once_and_retries() {
    let server = scenario_server(vec![
        Canned {
            status: 401,
            body: "first".to_owned(),
        },
        Canned {
            status: 200,
            body: EMBEDDING.to_owned(),
        },
    ])
    .await;
    let log = scratch("remint");
    let script = format!(
        "printf 'x\\n' >> '{}'; n=$(grep -c x '{}'); printf 'tok-%s\\n' \"$n\"",
        log.display(),
        log.display()
    );
    let built = provider(server.addr, Some(shell(&script)), None, None, None);
    let out = built
        .embed(vec!["q".to_owned()])
        .await
        .expect("the retry embeds");
    assert_eq!(out.len(), 1);
    let calls = server.recorder.lock().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls.first().and_then(|call| call.authorization.as_deref()),
        Some("Bearer tok-1")
    );
    assert_eq!(
        calls.get(1).and_then(|call| call.authorization.as_deref()),
        Some("Bearer tok-2")
    );
}

#[tokio::test]
async fn a_second_401_is_the_error() {
    let server = scenario_server(vec![
        Canned {
            status: 401,
            body: "first-denied".to_owned(),
        },
        Canned {
            status: 401,
            body: "second-denied".to_owned(),
        },
        Canned {
            status: 200,
            body: EMBEDDING.to_owned(),
        },
    ])
    .await;
    let log = scratch("reject");
    let script = format!(
        "printf 'x\\n' >> '{}'; n=$(grep -c x '{}'); printf 'tok-%s\\n' \"$n\"",
        log.display(),
        log.display()
    );
    let built = provider(server.addr, Some(shell(&script)), None, None, None);
    let error = built
        .embed(vec!["q".to_owned()])
        .await
        .expect_err("the second 401 stands");
    let ModelProviderError::Request { message, source } = error else {
        panic!("a status is a request error");
    };
    assert_eq!(message, "http 401");
    let rendered = source.to_string();
    assert!(rendered.contains("second-denied"), "{rendered}");
    assert!(!rendered.contains("first-denied"), "{rendered}");
    let embedded = built
        .embed(vec!["q".to_owned()])
        .await
        .expect("a later embed mints a new bearer");
    assert_eq!(embedded.len(), 1);
    let calls = server.recorder.lock().clone();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls.first().and_then(|call| call.authorization.as_deref()),
        Some("Bearer tok-1")
    );
    assert_eq!(
        calls.get(1).and_then(|call| call.authorization.as_deref()),
        Some("Bearer tok-2")
    );
    assert_eq!(
        calls.get(2).and_then(|call| call.authorization.as_deref()),
        Some("Bearer tok-3")
    );
    assert_eq!(lines(&log), 3);
}

#[tokio::test]
async fn list_models_and_chat_send_the_minted_bearer() {
    let server = scenario_server(vec![
        Canned {
            status: 200,
            body: MODEL_LIST.to_owned(),
        },
        Canned {
            status: 200,
            body: CHAT_SSE.to_owned(),
        },
    ])
    .await;
    let log = scratch("shared");
    let script = format!(
        "printf 'x\\n' >> '{}'; printf 'minted-token\\n'",
        log.display()
    );
    let built = provider(
        server.addr,
        Some(shell(&script)),
        Some("static-secret"),
        None,
        None,
    );
    let models = built.list_models().await.expect("models");
    assert_eq!(models, vec!["test-model".to_owned()]);
    let response = built.chat(hi_request()).await.expect("chat");
    assert_eq!(response.content.as_deref(), Some("hi"));
    let calls = server.recorder.lock().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(lines(&log), 1);
    for call in &calls {
        assert_eq!(call.authorization.as_deref(), Some("Bearer minted-token"));
    }
}

#[tokio::test]
async fn the_memory_embedder_sends_its_provider_bearer_and_remints_on_401() {
    let server = scenario_server(vec![
        Canned {
            status: 200,
            body: EMBEDDING.to_owned(),
        },
        Canned {
            status: 401,
            body: "expired".to_owned(),
        },
        Canned {
            status: 200,
            body: EMBEDDING.to_owned(),
        },
    ])
    .await;
    let log = scratch("embed-mints");
    let script = format!(
        "printf 'x\\n' >> '{}'; n=$(grep -c x '{}'); printf 'tok-%s\\n' \"$n\"",
        log.display(),
        log.display()
    );
    let endpoint = format!("http://{}", server.addr);
    let source = OpenaiProvider {
        base_url: Some(endpoint.clone()),
        api_key: Some("static-key".to_owned()),
        token_command: Some(shell(&script)),
        ..OpenaiProvider::default()
    };
    let embedder = common::demand(
        providers::embed::Embedder::build(providers::embed::EmbedConfig {
            endpoint,
            api_prefix: None,
            model: "test-model".to_owned(),
            api_key: None,
            expected_dims: None,
            timeout_ms: Some(5_000),
            query_instruction: None,
        })
        .and_then(|embedder| embedder.with_provider_auth(&source)),
        "the embedder builds",
    );
    for _ in 0..2 {
        let out = embedder
            .embed(vec!["q".to_owned()])
            .await
            .expect("the embed succeeds");
        assert_eq!(out.len(), 1);
    }
    let bearers: Vec<Option<String>> = server
        .recorder
        .lock()
        .iter()
        .map(|call| call.authorization.clone())
        .collect();
    assert_eq!(
        bearers,
        vec![
            Some("Bearer tok-1".to_owned()),
            Some("Bearer tok-1".to_owned()),
            Some("Bearer tok-2".to_owned()),
        ]
    );
    assert_eq!(lines(&log), 2);
}
