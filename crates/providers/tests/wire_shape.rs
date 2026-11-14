#[path = "common/mod.rs"]
#[expect(
    dead_code,
    reason = "the harness module is included whole by every test binary and this one uses a subset"
)]
mod common;

use common::demand;
use common::demand_some;
use common::generic_provider;
use common::scenario_record;
use common::scenario_server;
use kernel::model::ModelProvider;
use testkit::Json;
use testkit::WireDelta;

#[tokio::test]
async fn the_recorded_request_carries_exactly_the_table_fields() {
    let table = testkit::provider::B8_OMITTED_REQUEST_FIELDS;
    assert_eq!(
        table,
        &[
            ("temperature", WireDelta::SamplingOnModelServer),
            ("top_p", WireDelta::SamplingOnModelServer),
            ("max_tokens", WireDelta::OutputLimitOnModelServer),
            ("tool_choice", WireDelta::ServerDefaultToolChoice),
        ]
    );
    let raw_scenario =
        testkit::provider::load_scenario(&testkit::recording("providers/vllm/A-text"))
            .expect("the recorded scenario loads");
    let raw = raw_scenario
        .requests
        .first()
        .expect("the recorded request")
        .body
        .clone();
    let parsed: Json = serde_json::from_str(&raw).expect("recorded body parses");
    for (field, _) in table {
        assert!(
            parsed.get(field).is_some(),
            "the recorded body lost {field} before the table saw it"
        );
    }
    let stripped = testkit::strip_b8_deltas(&raw).expect("the recorded body strips");
    let stripped_json: Json = serde_json::from_str(&stripped).expect("stripped body parses");
    for (field, _) in table {
        assert!(
            stripped_json.get(field).is_none(),
            "{field} survived the strip"
        );
    }
    assert_eq!(stripped_json.get("stream"), Some(&Json::Bool(true)));
}

#[tokio::test]
async fn api_prefix_builds_the_request_target() {
    let recorded = scenario_record("A-text");
    let server = scenario_server(recorded.canned.clone()).await;
    let provider = config::provider::OpenaiProvider {
        model: Some("test-model".to_owned()),
        base_url: Some(format!("http://{}/", server.addr)),
        api_prefix: Some("openai/v1".to_owned()),
        ..config::provider::OpenaiProvider::default()
    };
    let built = providers::OpenAiCompatible::build(
        config::provider::ProviderType::OpenaiCompatible,
        &provider,
    )
    .expect("the generic provider builds");
    let body: Json = serde_json::from_str(&recorded.requests.first().expect("request").body)
        .expect("body parses");
    let mut rx = built
        .stream(common::model_request("A-text", &body))
        .await
        .expect("the stream starts");
    while rx.recv().await.is_some() {}
    let call = server
        .recorder
        .lock()
        .first()
        .cloned()
        .expect("the chat request is captured");
    assert_eq!(call.target, "/openai/v1/chat/completions");
    let sent: Json = serde_json::from_str(&call.body).expect("sent body parses");
    assert!(
        sent.get("chat_template_kwargs").is_none(),
        "the generic variant sends no vllm thinking flag"
    );
    assert!(
        sent.get("tool_choice").is_none(),
        "tool_choice is never sent"
    );
}

#[tokio::test]
async fn thinking_off_on_the_generic_variant_sends_no_vllm_flag() {
    let recorded = scenario_record("D-thinking-off");
    let server = scenario_server(recorded.canned.clone()).await;
    let built = generic_provider(server.addr);
    let body: Json = serde_json::from_str(&recorded.requests.first().expect("request").body)
        .expect("body parses");
    let request = common::model_request("D-thinking-off", &body);
    let mut rx = built.stream(request).await.expect("the stream starts");
    while rx.recv().await.is_some() {}
    let call = server
        .recorder
        .lock()
        .first()
        .cloned()
        .expect("the chat request is captured");
    let sent: Json = serde_json::from_str(&call.body).expect("sent body parses");
    assert!(
        sent.get("chat_template_kwargs").is_none(),
        "the thinking flag is a vllm behaviour"
    );
}

#[tokio::test]
async fn a_tool_without_a_description_is_sent_without_one() {
    let recorded = scenario_record("A-text");
    let server = scenario_server(recorded.canned.clone()).await;
    let provider = config::provider::OpenaiProvider {
        model: Some("test-model".to_owned()),
        base_url: Some(format!("http://{}", server.addr)),
        ..config::provider::OpenaiProvider::default()
    };
    let built = providers::OpenAiCompatible::build(
        config::provider::ProviderType::OpenaiCompatible,
        &provider,
    )
    .expect("the generic provider builds");
    let tool: rmcp::model::Tool = serde_json::from_str(
        "{\"name\":\"bare\",\"description\":\"has one\",\"inputSchema\":{\"type\":\"object\",\"properties\":{},\"required\":[]}}",
    )
    .expect("described tool parses");
    let mut undescribed = tool.clone();
    undescribed.description = None;
    let request = kernel::model::ModelRequest {
        options: kernel::model::ModelOptions::default(),
        model: "test-model".to_owned(),
        messages: Vec::new(),
        tools: vec![undescribed, tool],
        thinking: None,
        max_tokens: None,
    };
    let mut rx = built
        .stream(request)
        .await
        .expect("the undescribed tool is sent");
    while rx.recv().await.is_some() {}
    let call = server
        .recorder
        .lock()
        .first()
        .cloned()
        .expect("the chat request is captured");
    let sent: Json = serde_json::from_str(&call.body).expect("sent body parses");
    let expected: Json = serde_json::from_str(
        "[{\"type\":\"function\",\"function\":{\"name\":\"bare\",\"parameters\":{\"type\":\"object\",\"properties\":{},\"required\":[]}}},\
         {\"type\":\"function\",\"function\":{\"name\":\"bare\",\"description\":\"has one\",\"parameters\":{\"type\":\"object\",\"properties\":{},\"required\":[]}}}]",
    )
    .expect("expected tools parse");
    assert_eq!(sent.get("tools"), Some(&expected));
}

#[tokio::test]
async fn the_request_max_tokens_overrides_the_model_server_default() {
    let recorded = scenario_record("A-text");
    let server = scenario_server(recorded.canned.clone()).await;
    let provider = config::provider::OpenaiProvider {
        model: Some("test-model".to_owned()),
        base_url: Some(format!("http://{}", server.addr)),
        ..config::provider::OpenaiProvider::default()
    };
    let built = providers::OpenAiCompatible::build(
        config::provider::ProviderType::OpenaiCompatible,
        &provider,
    )
    .expect("the generic provider builds");
    let request = kernel::model::ModelRequest {
        options: kernel::model::ModelOptions {
            temperature: Some(0.2),
            enable_thinking: Some(false),
            json_schema: Some(kernel::wiki::builder::extraction_schema()),
            disable_any_whitespace: false,
        },
        model: "test-model".to_owned(),
        messages: Vec::new(),
        tools: Vec::new(),
        thinking: None,
        max_tokens: Some(999),
    };
    let mut rx = built.stream(request).await.expect("the stream starts");
    while rx.recv().await.is_some() {}
    let call = server
        .recorder
        .lock()
        .first()
        .cloned()
        .expect("the chat request is captured");
    let sent: wire::stream::openai::ChatCompletionRequest =
        serde_json::from_str(&call.body).expect("sent body parses");
    assert_eq!(sent.max_tokens, Some(999));
    assert_eq!(sent.temperature, Some(0.2));
    assert_eq!(sent.enable_thinking, Some(false));
    assert_eq!(
        sent.chat_template_kwargs,
        Some(wire::stream::openai::ChatTemplateKwargs {
            enable_thinking: false
        })
    );
    let format = sent.response_format.expect("a response format");
    assert_eq!(
        (format.kind.as_str(), format.json_schema.strict),
        ("json_schema", true)
    );
    assert_eq!(
        format.json_schema.schema,
        wire::JsonObject::from_serialize(&kernel::wiki::builder::extraction_schema())
            .expect("schema")
    );
}

#[tokio::test]
async fn openrouter_auth_and_reasoning_are_sent_only_when_configured() {
    let server = openrouter_server().await;
    let request = openrouter_request();
    drain_openrouter(
        &openrouter_provider(&server, Some("test-openrouter-key"), None, Some(false)),
        &request,
    )
    .await;
    drain_openrouter(&openrouter_provider(&server, None, None, None), &request).await;
    let dir = tempfile::tempdir().expect("key dir");
    let path = dir.path().join("openrouter_api_key");
    std::fs::write(&path, "file-key\n").expect("key file");
    drain_openrouter(
        &openrouter_provider(&server, None, Some(path), Some(false)),
        &request,
    )
    .await;
    assert_openrouter_shapes(&server.recorder.lock());
}

async fn openrouter_server() -> common::ScenarioServer {
    let recorded = scenario_record("A-text");
    let mut canned = recorded.canned.clone();
    canned.extend(recorded.canned.iter().cloned());
    canned.extend(recorded.canned.iter().cloned());
    scenario_server(canned).await
}

fn openrouter_request() -> kernel::model::ModelRequest {
    kernel::model::ModelRequest {
        options: kernel::model::ModelOptions {
            enable_thinking: Some(false),
            json_schema: Some(kernel::wiki::builder::extraction_schema()),
            disable_any_whitespace: false,
            ..kernel::model::ModelOptions::default()
        },
        model: "qwen/qwen3.8-flash".to_owned(),
        messages: Vec::new(),
        tools: Vec::new(),
        thinking: None,
        max_tokens: Some(64),
    }
}

fn openrouter_provider(
    server: &common::ScenarioServer,
    api_key: Option<&str>,
    api_key_file: Option<std::path::PathBuf>,
    reasoning: Option<bool>,
) -> config::provider::OpenaiProvider {
    config::provider::OpenaiProvider {
        model: Some("qwen/qwen3.8-flash".to_owned()),
        base_url: Some(format!("http://{}", server.addr)),
        api_key: api_key.map(str::to_owned),
        api_key_file,
        reasoning,
        ..config::provider::OpenaiProvider::default()
    }
}

async fn drain_openrouter(
    provider: &config::provider::OpenaiProvider,
    request: &kernel::model::ModelRequest,
) {
    let built = demand(
        providers::OpenAiCompatible::build(
            config::provider::ProviderType::OpenaiCompatible,
            provider,
        ),
        "provider",
    );
    let mut rx = demand(built.stream(request.clone()).await, "stream");
    while rx.recv().await.is_some() {}
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        untyped,
        reason = "tool arguments and model JSON are arbitrary objects"
    )
)]
fn assert_openrouter_shapes(calls: &[common::CapturedCall]) {
    let configured = demand_some(calls.first(), "configured call");
    assert_eq!(
        configured.authorization.as_deref(),
        Some("Bearer test-openrouter-key")
    );
    assert_reasoning(&configured.body, Some(false));
    assert_thinking(&configured.body);
    let configured_body = json_body(&configured.body);
    assert_eq!(
        configured_body
            .get("response_format")
            .and_then(|value| value.get("type"))
            .and_then(serde_json::Value::as_str),
        Some("json_schema")
    );
    let plain = demand_some(calls.get(1), "plain call");
    assert_eq!(plain.authorization, None);
    assert_reasoning(&plain.body, None);
    assert_thinking(&plain.body);
    let from_file = demand_some(calls.get(2), "file call");
    assert_eq!(from_file.authorization.as_deref(), Some("Bearer file-key"));
    assert_reasoning(&from_file.body, Some(false));
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        untyped,
        reason = "tool arguments and model JSON are arbitrary objects"
    )
)]
fn assert_reasoning(body: &str, enabled: Option<bool>) {
    let sent = json_body(body)
        .get("reasoning")
        .and_then(|value| value.get("enabled"))
        .and_then(serde_json::Value::as_bool);
    assert_eq!(sent, enabled);
}

fn assert_thinking(body: &str) {
    assert_eq!(
        json_body(body)
            .get("chat_template_kwargs")
            .and_then(|value| value.get("enable_thinking")),
        Some(&serde_json::Value::Bool(false))
    );
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        untyped,
        reason = "tool arguments and model JSON are arbitrary objects"
    )
)]
fn json_body(body: &str) -> serde_json::Value {
    demand(serde_json::from_str(body), "body")
}
