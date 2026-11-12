use crate::common;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use gateway::Gateway;
use gateway::plugins::{HostSettings, PluginHost};
use gateway::tenancy::{Tenancy, UserId};
use kernel::memory::share::desk::ShareDesk;
use serde::Serialize;
use tower::ServiceExt as _;
use wire::JsonObject;
use wire::gateway::GatewayError;
use wire::plugin::host::{PluginEnableRequest, PluginInstallRequest, PluginRecord};
use wire::plugin::manifest::{Permission, PluginManifest};
use wire::plugin::process::{PluginToolCall, PluginToolResult};

pub(crate) type Failure = Box<dyn std::error::Error + Send + Sync>;
pub(crate) type TestResult = Result<(), Failure>;

pub(crate) const POLL: Duration = Duration::from_millis(50);
pub(crate) const OWNER_DEVICE: &str = "laptop";

const MANIFEST: &str = r#"{
  "manifest": 1,
  "id": "fixture",
  "name": "Fixture",
  "version": "0.1.0",
  "description": "A plug-in for the host tests.",
  "run": { "command": ["COMMAND"], "readyTimeoutMs": 5000 },
  "permissions": ["sessions.read", "topics.read"],
  "tools": [
    { "name": "echo", "description": "Echo the arguments.", "inputSchema": { "type": "object" } },
    { "name": "events", "description": "List received events.", "inputSchema": { "type": "object" } },
    { "name": "pid", "description": "Answer the process id.", "inputSchema": { "type": "object" } },
    { "name": "api", "description": "Call the plug-in API.", "inputSchema": { "type": "object" } },
    { "name": "read", "description": "Read a file.", "inputSchema": { "type": "object" } },
    { "name": "connect", "description": "Open a TCP connection.", "inputSchema": { "type": "object" } }
  ],
  "settings": [
    { "key": "runs", "label": "Runs at once", "input": { "type": "number", "min": 1, "max": 8 }, "value": 2, "required": true },
    { "key": "token", "label": "Token", "input": { "type": "secret" } }
  ],
  "contributes": { "pages": [{ "id": "fixture", "title": "Fixture", "view": "status" }] },
  "jobs": [{ "id": "tick", "title": "Tick" }],
  "events": ["session.idle"]
}"#;

pub(crate) fn invalid_manifest() -> String {
    MANIFEST.replace(r#""topics.read"]"#, r#""topics.read"], "colour": "red""#)
}

#[derive(Serialize)]
struct Probe<'a> {
    method: &'a str,
    path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a JsonObject>,
}

pub(crate) struct Answer {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
}

pub(crate) struct Harness {
    pub(crate) app: Router,
    pub(crate) gateway: Arc<Gateway>,
    pub(crate) store: Arc<testkit::FakeMemoryStore>,
}

impl Harness {
    pub(crate) async fn send(&self, mut request: Request<Body>) -> Result<Answer, Failure> {
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:41000".parse::<SocketAddr>()?));
        common::authorize(&mut request);
        let response =
            tokio::time::timeout(Duration::from_secs(20), self.app.clone().oneshot(request))
                .await??;
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024).await?;
        Ok(Answer {
            status,
            headers,
            body,
        })
    }

    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<String>,
        device: bool,
    ) -> Result<(StatusCode, Bytes), Failure> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:5174")
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, Body::from))?;
        if device {
            request.headers_mut().insert(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_str(&format!("Bearer {}", OWNER_DEVICE.to_owned()))?,
            );
        }
        let answer = self.send(request).await?;
        Ok((answer.status, answer.body))
    }

    pub(crate) async fn record(&self) -> Result<PluginRecord, Failure> {
        let (_, bytes) = self
            .call(Method::GET, "/api/plugins/fixture", None, false)
            .await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub(crate) async fn until(
        &self,
        what: &str,
        mut ready: impl FnMut(&PluginRecord) -> bool,
    ) -> Result<PluginRecord, Failure> {
        let mut poll = tokio::time::interval(POLL);
        for _ in 0..400 {
            poll.tick().await;
            let record = self.record().await?;
            if ready(&record) {
                return Ok(record);
            }
        }
        Err(format!("the plug-in never reached {what}").into())
    }

    pub(crate) async fn tool(
        &self,
        name: &str,
        arguments: JsonObject,
    ) -> Result<PluginToolResult, Failure> {
        let call = PluginToolCall {
            arguments,
            session_id: None,
        };
        let (status, bytes) = self
            .call(
                Method::POST,
                &format!("/api/plugins/fixture/tools/{name}"),
                Some(serde_json::to_string(&call)?),
                false,
            )
            .await?;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub(crate) async fn probe(&self, method: &str, path: &str) -> Result<String, Failure> {
        let answer = self.probe_body(method, path).await?;
        Ok(answer
            .split_once(' ')
            .map_or_else(|| answer.clone(), |(status, _)| status.to_owned()))
    }

    pub(crate) async fn probe_body(&self, method: &str, path: &str) -> Result<String, Failure> {
        self.probe_with(method, path, None).await
    }

    pub(crate) async fn probe_with(
        &self,
        method: &str,
        path: &str,
        body: Option<&JsonObject>,
    ) -> Result<String, Failure> {
        let probe = Probe { method, path, body };
        Ok(self
            .tool("api", JsonObject::from_serialize(&probe)?)
            .await?
            .text)
    }

    pub(crate) async fn install_and_enable(&self, source: &Path) -> Result<PluginRecord, Failure> {
        let install = serde_json::to_string(&PluginInstallRequest {
            path: source.display().to_string(),
        })?;
        let (status, bytes) = self
            .call(Method::POST, "/api/plugins", Some(install), false)
            .await?;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let record: PluginRecord = serde_json::from_slice(&bytes)?;
        let (status, bytes) = self
            .call(
                Method::POST,
                "/api/plugins/fixture/enable",
                enable(&record.pending)?,
                true,
            )
            .await?;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        self.until("running", |record| {
            record.state == wire::plugin::host::PluginState::Running
        })
        .await
    }
}

pub(crate) fn write_manifest(source: &Path, permissions: &[Permission]) -> Result<(), Failure> {
    write_manifest_with(source, permissions, |_| {})
}

pub(crate) fn write_manifest_with(
    source: &Path,
    permissions: &[Permission],
    adjust: impl FnOnce(&mut PluginManifest),
) -> Result<(), Failure> {
    std::fs::create_dir_all(source)?;
    let program = source.join("fixture-plugin");
    if !program.exists() {
        std::fs::copy(env!("CARGO_BIN_EXE_fixture-plugin"), &program)?;
    }
    let text = MANIFEST.replace("COMMAND", "./fixture-plugin");
    let mut manifest: PluginManifest = serde_json::from_str(&text)?;
    manifest.permissions = permissions.to_vec();
    adjust(&mut manifest);
    std::fs::write(
        source.join("rivethub-plugin.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

pub(crate) fn owner() -> Result<UserId, Failure> {
    Ok(UserId::new("owner".to_owned())?)
}

pub(crate) fn tenancy() -> Result<Tenancy, Failure> {
    let owner = owner()?;
    Ok(Tenancy::local(owner.clone()).with_token(OWNER_DEVICE, owner)?)
}

pub(crate) async fn harness(root: &Path) -> Result<Harness, Failure> {
    harness_with(root, tenancy()?, Vec::new()).await
}

pub(crate) async fn harness_with(
    root: &Path,
    tenancy: Tenancy,
    node_ports: Vec<u16>,
) -> Result<Harness, Failure> {
    open_harness(root, tenancy, node_ports, None).await
}

pub(crate) async fn harness_with_desk(
    root: &Path,
    desk: Arc<dyn ShareDesk>,
) -> Result<Harness, Failure> {
    open_harness(root, tenancy()?, Vec::new(), Some(desk)).await
}

async fn open_harness(
    root: &Path,
    tenancy: Tenancy,
    node_ports: Vec<u16>,
    desk: Option<Arc<dyn ShareDesk>>,
) -> Result<Harness, Failure> {
    let mut settings = common::settings();
    settings.state_dir = root.join("state");
    let host = PluginHost::open(HostSettings {
        root: root.join("state").join("plugins"),
        workshop: root.join("state").join("workshop"),
        templates: root.join("state").join("plugin-templates"),
        node: "node-a".to_owned(),
        inherited: Vec::new(),
        node_ports,
    })
    .await?;
    let store = Arc::new(testkit::FakeMemoryStore::default());
    let mut gateway = Gateway::new(
        Arc::clone(&store) as Arc<dyn kernel::memory::MemoryStore>,
        settings,
    )
    .with_local_owner(crate::common::local_owner())
    .with_tenancy(tenancy)
    .with_plugins(host);
    if let Some(desk) = desk {
        gateway = gateway.with_share_desk(desk);
    }
    let gateway = Arc::new(gateway);
    gateway::plugins::start(&gateway).await?;
    Ok(Harness {
        app: gateway.router(false),
        gateway,
        store,
    })
}

pub(crate) fn refused(bytes: &Bytes) -> Result<GatewayError, Failure> {
    Ok(serde_json::from_slice(bytes)?)
}

pub(crate) fn enable(approve: &[Permission]) -> Result<Option<String>, Failure> {
    Ok(Some(serde_json::to_string(&PluginEnableRequest {
        approve: approve.to_vec(),
    })?))
}
