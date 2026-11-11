use std::sync::Arc;

use rmcp::ClientHandler;
use rmcp::ErrorData;
use rmcp::RoleClient;
use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResult;
use rmcp::model::ClientCapabilities;
use rmcp::model::ClientConfig;
use rmcp::model::ElicitRequestParams;
use rmcp::model::ElicitResult;
use rmcp::model::ElicitationAction;
use rmcp::model::ElicitationCapability;
use rmcp::model::Implementation;
use rmcp::service::RequestContext;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::fixture::SHARE_LINE;
use super::fixture::node;
use super::fixture::sessions;
use super::fixture::text;
use super::sanitized;

#[derive(Clone)]
struct Human {
    elicitation: bool,
    answer: Option<(ElicitationAction, &'static str)>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl ClientHandler for Human {
    fn get_info(&self) -> ClientConfig {
        let mut capabilities = ClientCapabilities::default();
        if self.elicitation {
            capabilities.elicitation = Some(ElicitationCapability::new());
        }
        ClientConfig::new(capabilities, Implementation::new("test-client", "1"))
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        if let ElicitRequestParams::FormElicitationParams { message, .. } = request {
            self.asked.lock().await.push(message);
        }
        let (action, content) = self
            .answer
            .clone()
            .unwrap_or((ElicitationAction::Decline, ""));
        let mut result = ElicitResult::new(action);
        if !content.is_empty() {
            result.content = Some(serde_json::from_str(content).expect("answer"));
        }
        Ok(result)
    }
}

async fn share_over_stdio(server: mcp::Server, human: Human) -> CallToolResult {
    let (client_io, server_io) = tokio::io::duplex(65536);
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(mcp::transport::serve_io(
        server,
        server_io,
        shutdown.clone(),
    ));
    let client = human.serve(client_io).await.expect("initialize");
    let mut request = CallToolRequestParams::new("session_share".to_owned());
    request.arguments = serde_json::from_str(r#"{"bundle":"work.clean.json"}"#).expect("arguments");
    let result = client.call_tool(request).await.expect("tool result");
    client.cancel().await.expect("client closed");
    shutdown.cancel();
    task.await.expect("task").expect("server closed");
    result
}

fn human(elicitation: bool, answer: Option<(ElicitationAction, &'static str)>) -> Human {
    Human {
        elicitation,
        answer,
        asked: Arc::new(Mutex::new(Vec::new())),
    }
}

#[tokio::test]
async fn share_refuses_without_elicitation() {
    let phil = node("phil", sessions(), None).await;
    let clean = sanitized(&phil).await;
    let asker = human(false, None);
    let result = share_over_stdio(phil.server, asker.clone()).await;
    assert_eq!(result.is_error, Some(true));
    assert!(
        text(&result).contains("ask the user to run `rh memory share <path>`"),
        "{}",
        text(&result)
    );
    assert_eq!(asker.asked.lock().await.len(), 0);
    assert_eq!(
        phil.share_port.uploads().await,
        Vec::<std::path::PathBuf>::new()
    );
    assert!(clean.exists());
}

#[tokio::test]
async fn a_declined_elicitation_uploads_nothing() {
    for answer in [
        None,
        Some((ElicitationAction::Cancel, "")),
        Some((ElicitationAction::Accept, r#"{"share":false}"#)),
        Some((ElicitationAction::Accept, "{}")),
    ] {
        let phil = node("phil", sessions(), None).await;
        sanitized(&phil).await;
        let asker = human(true, answer.clone());
        let port = Arc::clone(&phil.share_port);
        let result = share_over_stdio(phil.server, asker.clone()).await;
        assert_eq!(result.is_error, Some(true), "{answer:?}");
        assert!(text(&result).starts_with("not shared: the user did not confirm"));
        assert_eq!(
            port.uploads().await,
            Vec::<std::path::PathBuf>::new(),
            "{answer:?}"
        );
        let prompt = asker
            .asked
            .lock()
            .await
            .first()
            .cloned()
            .expect("the human was asked");
        assert!(
            prompt.starts_with("Share 2 sessions by phil (signed ed25519:0011223344556677, 2 KiB)"),
            "{prompt}"
        );
        assert!(prompt.contains("expires 7 days after upload; send it privately"));
    }
}

#[tokio::test]
async fn an_accepted_elicitation_returns_the_code_once() {
    let phil = node("phil", sessions(), None).await;
    let clean = sanitized(&phil).await;
    let port = Arc::clone(&phil.share_port);
    let asker = human(true, Some((ElicitationAction::Accept, r#"{"share":true}"#)));
    let result = share_over_stdio(phil.server, asker).await;
    assert_ne!(result.is_error, Some(true), "{}", text(&result));
    assert_eq!(
        text(&result),
        format!(
            "shared 2 sessions; {SHARE_LINE}\nNo tool sends, posts or stores this code: give it to the user to send privately.\n"
        )
    );
    assert_eq!(port.uploads().await, [clean]);
}

#[tokio::test]
async fn no_tool_accepts_a_code() {
    let phil = node("phil", sessions(), None).await;
    for tool in phil.server.tools() {
        let schema = serde_json::to_string(&tool.input_schema).expect("schema");
        assert!(!schema.contains("\"code\""), "{} takes a code", tool.name);
        assert!(!tool.name.contains("receive"), "{} receives", tool.name);
    }
    let result = super::support::call(
        &phil.server,
        "session_share",
        r#"{"bundle":"work.clean.json","code":"rhb1:AAAAAAAAAAAAAAAAAAAAAA#qqqq"}"#,
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert!(text(&result).contains("unknown field"), "{}", text(&result));
}
