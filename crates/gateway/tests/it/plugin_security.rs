use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use wire::JsonObject;
use wire::capture::{CaptureBatchRequest, CaptureMessage};
use wire::gateway::memory::MemoryAppendToolArgs;
use wire::message::Role;
use wire::plugin::host::{PluginInstallRequest, PluginLogs, PluginState};
use wire::plugin::manifest::{Permission, PluginManifest};

use gateway::tenancy::{PeerCertificate, Tenancy, UserId};

use crate::plugin_support::{
    Failure, Harness, OWNER_DEVICE, POLL, TestResult, enable, harness, harness_with, owner,
    refused, tenancy, write_manifest, write_manifest_with,
};

const BASE: [Permission; 2] = [Permission::SessionsRead, Permission::TopicsRead];

fn get(path: &str) -> Result<Request<Body>, axum::http::Error> {
    Request::builder()
        .method(Method::GET)
        .uri(path)
        .header("host", "127.0.0.1:5174")
        .body(Body::empty())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_page_is_kept_off_the_gateway_origin() -> TestResult {
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    write_manifest(&source, &BASE)?;
    let host = harness(root.path()).await?;
    host.install_and_enable(&source).await?;
    for path in [
        "/api/plugins/fixture/http/hello",
        "/api/plugins/fixture/panel/index.html",
    ] {
        let answer = host.send(get(path)?).await?;
        let sandboxed = answer
            .headers
            .get_all("content-security-policy")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|policy| policy.starts_with("sandbox ") && !policy.contains("allow-same-origin"));
        assert!(sandboxed, "{path}: {:?}", answer.headers);
        assert_eq!(
            answer
                .headers
                .get("x-content-type-options")
                .map(axum::http::HeaderValue::as_bytes),
            Some(&b"nosniff"[..]),
            "{path}"
        );
    }
    host.gateway.close().await;
    Ok(())
}

async fn stopping(host: &Harness) -> TestResult {
    let mut poll = tokio::time::interval(POLL);
    for _ in 0..200 {
        poll.tick().await;
        let (_, bytes) = host
            .call(Method::GET, "/api/plugins/fixture/logs", None, false)
            .await?;
        let logs: PluginLogs = serde_json::from_slice(&bytes)?;
        if logs
            .lines
            .iter()
            .any(|line| line.text.contains("ignored SIGTERM"))
        {
            return Ok(());
        }
    }
    Err("the reload never stopped the plug-in".into())
}

fn slow_to_stop(manifest: &mut PluginManifest) {
    manifest
        .run
        .env
        .insert("FIXTURE_IGNORE_SIGTERM".to_owned(), "1".to_owned());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reload_queued_ahead_of_enable_cannot_slip_a_permission_past_the_device_check()
-> TestResult {
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    write_manifest_with(&source, &BASE, slow_to_stop)?;
    let host = Arc::new(harness(root.path()).await?);
    host.install_and_enable(&source).await?;
    let wider = [BASE[0], BASE[1], Permission::AgentsRead];
    write_manifest_with(&source, &wider, slow_to_stop)?;
    let reloading = tokio::spawn({
        let host = Arc::clone(&host);
        async move {
            host.call(Method::POST, "/api/plugins/fixture/reload", None, false)
                .await
        }
    });
    stopping(&host).await?;
    let (status, bytes) = host
        .call(
            Method::POST,
            "/api/plugins/fixture/enable",
            enable(&[Permission::AgentsRead])?,
            false,
        )
        .await?;
    let (reloaded, _) = reloading.await??;
    assert_eq!(reloaded, StatusCode::OK);
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(
        refused(&bytes)?.code.as_deref(),
        Some("device_certificate_required")
    );
    let record = host.record().await?;
    assert!(!record.granted.contains(&Permission::AgentsRead));
    assert_eq!(record.state, PluginState::AwaitingApproval);
    host.gateway.close().await;
    Ok(())
}

fn post(path: &str) -> Result<Request<Body>, axum::http::Error> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("host", "127.0.0.1:5174")
        .body(Body::empty())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proxied_path_cannot_climb_out_of_its_route() -> TestResult {
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    write_manifest(&source, &BASE)?;
    let host = harness(root.path()).await?;
    host.install_and_enable(&source).await?;
    for path in [
        "/api/plugins/fixture/http/../jobs/tick",
        "/api/plugins/fixture/http/%2e%2e/jobs/tick",
        "/api/plugins/fixture/http/.%2E/jobs/tick",
        "/api/plugins/fixture/panel/a/%2E%2E/%2e%2e/jobs/tick",
        "/api/plugins/fixture/http/..%2Fjobs/tick",
    ] {
        let answer = host.send(post(path)?).await?;
        assert_eq!(
            answer.status,
            StatusCode::BAD_REQUEST,
            "{path}: {}",
            String::from_utf8_lossy(&answer.body)
        );
    }
    host.gateway.close().await;
    Ok(())
}

fn forged(device: &str) -> Result<Request<Body>, Failure> {
    let mut request = Request::builder()
        .method(Method::GET)
        .uri("/api/plugins/fixture/http/headers")
        .header("host", "127.0.0.1:5174")
        .header("x-rivethub-user", "owner")
        .header("x-rivethub-forged", "yes")
        .body(Body::empty())?;
    request.extensions_mut().insert(PeerCertificate::Device {
        user: None,
        device: device.to_owned(),
    });
    Ok(request)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_caller_cannot_forge_the_headers_rh_names_it_with() -> TestResult {
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    write_manifest(&source, &BASE)?;
    let owner = owner()?;
    let tenancy = Tenancy::local(owner.clone())
        .with_device(OWNER_DEVICE, owner)?
        .with_device("phone", UserId::new("zoë".to_owned())?)?;
    let host = harness_with(root.path(), tenancy, Vec::new()).await?;
    host.install_and_enable(&source).await?;
    let answer = host.send(forged(OWNER_DEVICE)?).await?;
    let seen = String::from_utf8_lossy(&answer.body);
    assert_eq!(answer.status, StatusCode::OK, "{seen}");
    assert!(seen.contains("x-rivethub-user=owner"), "{seen}");
    assert!(!seen.contains("x-rivethub-forged"), "{seen}");
    let answer = host.send(forged("phone")?).await?;
    let seen = String::from_utf8_lossy(&answer.body);
    assert_eq!(answer.status, StatusCode::OK, "{seen}");
    assert!(seen.contains("x-rivethub-user=zoë"), "{seen}");
    assert!(
        !seen.contains("=owner") && !seen.contains("forged"),
        "{seen}"
    );
    host.gateway.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_writes_memory_only_under_its_own_session_keys() -> TestResult {
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    write_manifest(&source, &[BASE[0], BASE[1], Permission::MemoryWrite])?;
    let host = harness(root.path()).await?;
    host.install_and_enable(&source).await?;
    let session = "claude:the-owners-session".to_owned();
    let append = JsonObject::from_serialize(&MemoryAppendToolArgs {
        session_id: session.clone(),
        content: "planted".into(),
        role: Role::User,
        tool_name: None,
        tool_args: None,
        tool_result: None,
        event_id: None,
        agent: None,
        persona: None,
        source: None,
        channel: None,
    })?;
    let capture = JsonObject::from_serialize(&CaptureBatchRequest {
        session_key: session,
        agent: "fixture".into(),
        channel: None,
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![CaptureMessage {
            event_id: "planted-1".into(),
            role: Role::User,
            content: "planted".into(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: None,
        }],
    })?;
    for (path, body) in [
        ("/api/memory/tool/memory_append", &append),
        ("/api/capture", &capture),
    ] {
        let answer = host.probe_with("POST", path, Some(body)).await?;
        assert!(answer.starts_with("200 "), "{path}: {answer}");
    }
    let keys: Vec<String> = host
        .store
        .requests()
        .await
        .into_iter()
        .flat_map(|request| {
            if let testkit::MemoryRequest::Append(_, entries) = request {
                entries
            } else {
                Vec::new()
            }
        })
        .map(|entry| entry.session_id)
        .collect();
    assert_eq!(
        keys, ["plugin:fixture:claude:the-owners-session"; 2],
        "{keys:?}"
    );
    host.gateway.close().await;
    Ok(())
}

#[derive(serde::Serialize)]
struct PortProbe {
    port: u16,
}

#[tokio::test(flavor = "multi_thread")]
async fn a_networked_plugin_cannot_reach_the_nodes_own_listeners() -> TestResult {
    let gateway = std::net::TcpListener::bind("127.0.0.1:0")?;
    let elsewhere = std::net::TcpListener::bind("127.0.0.1:0")?;
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    let asked = [BASE[0], BASE[1], Permission::HostNetwork];
    write_manifest(&source, &asked)?;
    let host = harness_with(root.path(), tenancy()?, vec![gateway.local_addr()?.port()]).await?;
    let install = serde_json::to_string(&PluginInstallRequest {
        path: source.display().to_string(),
    })?;
    host.call(Method::POST, "/api/plugins", Some(install), false)
        .await?;
    let (status, bytes) = host
        .call(
            Method::POST,
            "/api/plugins/fixture/enable",
            enable(&asked)?,
            true,
        )
        .await?;
    if status == StatusCode::CONFLICT {
        assert_eq!(
            refused(&bytes)?.code.as_deref(),
            Some("sandbox_unavailable")
        );
        host.gateway.close().await;
        return Ok(());
    }
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    host.until("running", |record| record.state == PluginState::Running)
        .await?;
    let connect = |port: u16| JsonObject::from_serialize(&PortProbe { port });
    let refused_port = host
        .tool("connect", connect(gateway.local_addr()?.port())?)
        .await?;
    assert!(refused_port.is_error, "{}", refused_port.text);
    let open = host
        .tool("connect", connect(elsewhere.local_addr()?.port())?)
        .await?;
    assert_eq!(open.text, "connected");
    host.gateway.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_caller_holding_the_tools_token_reaches_the_tools_socket() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir()?;
    let host = harness(root.path()).await?;
    let folder = root.path().join("state/plugins");
    assert_eq!(
        std::fs::metadata(&folder)?.permissions().mode() & 0o777,
        0o700
    );
    let tools = reqwest::Client::builder()
        .unix_socket(folder.join("tools.sock"))
        .build()?;
    let anonymous = tools.get("http://plugins/tools").send().await?;
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let token = std::fs::read_to_string(folder.join("tools.token"))?;
    let held = tools
        .get("http://plugins/tools")
        .bearer_auth(token.trim())
        .send()
        .await?;
    assert_eq!(held.status(), StatusCode::OK);
    host.gateway.close().await;
    Ok(())
}
