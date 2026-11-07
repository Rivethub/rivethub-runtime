use std::path::Path;

use axum::http::{Method, StatusCode};
use serde::Serialize;
use wire::JsonObject;
use wire::harness::HarnessId;
use wire::plugin::action::{ViewActionRefusal, ViewActionResult};
use wire::plugin::host::{
    PluginInstallRequest, PluginJobOutcome, PluginLogStream, PluginLogs, PluginRecord,
    PluginSettings, PluginState,
};
use wire::plugin::manifest::Permission;
use wire::plugin::process::PluginEvent;
use wire::plugin::view::{View, ViewValue};
use wire::session_id::SessionId;

use crate::plugin_support::{
    Failure, Harness, POLL, TestResult, enable, harness, invalid_manifest, refused, write_manifest,
};

#[derive(Serialize)]
struct FileProbe<'a> {
    path: &'a Path,
}

#[tokio::test(flavor = "multi_thread")]
async fn an_installed_plugin_is_approved_by_the_owner_and_served_through_the_proxy() -> TestResult {
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    write_manifest(&source, &[Permission::SessionsRead, Permission::TopicsRead])?;
    let host = harness(root.path()).await?;
    install_and_approve(&host, &source).await?;
    proxied_routes_are_checked(&host).await?;
    api_calls_are_scoped(&host).await?;
    the_process_is_confined_to_its_folders(&host, root.path()).await?;
    settings_are_checked_kept_and_delivered(&host).await?;
    events_and_jobs_reach_the_process(&host).await?;
    a_killed_process_is_restarted(&host).await?;
    a_wider_manifest_waits_for_approval(&host, &source).await?;
    disable_and_remove(&host, root.path()).await?;
    host.gateway.close().await;
    Ok(())
}

async fn install_and_approve(host: &Harness, source: &Path) -> TestResult {
    let install = serde_json::to_string(&PluginInstallRequest {
        path: source.display().to_string(),
    })?;
    let (status, bytes) = host
        .call(Method::POST, "/api/plugins", Some(install.clone()), false)
        .await?;
    assert_eq!(status, StatusCode::CREATED);
    let record: PluginRecord = serde_json::from_slice(&bytes)?;
    assert_eq!(record.state, PluginState::Disabled);
    assert_eq!(
        record.pending,
        [Permission::SessionsRead, Permission::TopicsRead]
    );
    let (status, _) = host
        .call(Method::POST, "/api/plugins", Some(install), false)
        .await?;
    assert_eq!(status, StatusCode::CONFLICT);
    let everything = [Permission::SessionsRead, Permission::TopicsRead];
    let (status, bytes) = host
        .call(
            Method::POST,
            "/api/plugins/fixture/enable",
            enable(&[Permission::SessionsRead])?,
            true,
        )
        .await?;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        refused(&bytes)?.code.as_deref(),
        Some("permissions_unapproved")
    );
    let (status, _) = host
        .call(
            Method::POST,
            "/api/plugins/fixture/enable",
            enable(&everything)?,
            true,
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    host.until("running", |record| record.state == PluginState::Running)
        .await?;
    Ok(())
}

async fn proxied_routes_are_checked(host: &Harness) -> TestResult {
    let post = |path: &'static str| host.call(Method::POST, path, Some("{}".into()), false);
    let (status, bytes) = post("/api/plugins/fixture/views/status").await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(serde_json::from_slice::<View>(&bytes)?.title, "Fixture");
    let (status, bytes) = post("/api/plugins/fixture/views/broken").await?;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let broken = refused(&bytes)?;
    assert_eq!(broken.code.as_deref(), Some("plugin_invalid_response"));
    assert_eq!(
        broken.detail.as_deref(),
        Some("blocks[0]: missing field `text`")
    );
    let (status, bytes) = post("/api/plugins/fixture/views/nested").await?;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        refused(&bytes)?.detail.as_deref(),
        Some("blocks[1].items[1].title: invalid type: integer `42`, expected a string")
    );
    let (status, bytes) = post("/api/plugins/fixture/actions/save").await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<ViewActionResult>(&bytes)?
            .actions
            .len(),
        1
    );
    let (status, bytes) = post("/api/plugins/fixture/actions/refuse").await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        serde_json::from_slice::<ViewActionRefusal>(&bytes)?.message,
        "refused"
    );
    let (status, _) = post("/api/plugins/fixture/actions/loop").await?;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let (status, bytes) = host
        .call(Method::GET, "/api/plugins/fixture/http/hello", None, false)
        .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&bytes[..], b"hello from the fixture plug-in");
    Ok(())
}

async fn api_calls_are_scoped(host: &Harness) -> TestResult {
    let own = host.probe_body("GET", "/api/plugin/self").await?;
    assert!(own.starts_with("200 "), "{own}");
    assert_eq!(host.probe("GET", "/api/agents").await?, "403");
    assert_eq!(host.probe("GET", "/api/plugins").await?, "403");
    assert_eq!(
        host.probe("POST", "/api/plugin/views/changed").await?,
        "204"
    );
    Ok(())
}

async fn the_process_is_confined_to_its_folders(host: &Harness, root: &Path) -> TestResult {
    let outside = root.join("outside.txt");
    std::fs::write(&outside, "not the plug-in's")?;
    let inside = root.join("state/plugins/fixture/data/inside.txt");
    std::fs::write(&inside, "the plug-in's own")?;
    let read = |path: &Path| -> Result<JsonObject, Failure> {
        Ok(JsonObject::from_serialize(&FileProbe { path })?)
    };
    let own = host.tool("read", read(&inside)?).await?;
    assert_eq!(own.text, "the plug-in's own");
    let (_, bytes) = host
        .call(Method::GET, "/api/plugins/fixture/logs", None, false)
        .await?;
    let logs: PluginLogs = serde_json::from_slice(&bytes)?;
    let confined = logs.lines.iter().any(|line| {
        line.stream == PluginLogStream::Host && line.text.contains("confined by Landlock")
    });
    let other = host.tool("read", read(&outside)?).await?;
    assert_eq!(other.is_error, confined, "{}", other.text);
    Ok(())
}

async fn settings_are_checked_kept_and_delivered(host: &Harness) -> TestResult {
    let (status, bytes) = host
        .call(Method::GET, "/api/plugins/fixture/settings", None, false)
        .await?;
    assert_eq!(status, StatusCode::OK);
    let shown: PluginSettings = serde_json::from_slice(&bytes)?;
    assert_eq!(shown.values.get("runs"), Some(&ViewValue::Number(2.0)));
    let put = |body: &str| {
        host.call(
            Method::PUT,
            "/api/plugins/fixture/settings",
            Some(body.to_owned()),
            false,
        )
    };
    let (status, bytes) = put(r#"{"values": {"runs": 9}}"#).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(refused(&bytes)?.error.starts_with("runs: "));
    let (status, bytes) = put(r#"{"values": {"runs": 4, "token": "s3cret"}}"#).await?;
    assert_eq!(status, StatusCode::OK);
    let kept: PluginSettings = serde_json::from_slice(&bytes)?;
    assert_eq!(kept.values.get("runs"), Some(&ViewValue::Number(4.0)));
    assert!(!kept.values.contains_key("token"));
    assert_eq!(kept.secrets_set, ["token"]);
    let own = host.probe_body("GET", "/api/plugin/self").await?;
    assert!(own.contains(r#""token":"s3cret""#), "{own}");
    let mut delivered = String::new();
    let mut poll = tokio::time::interval(POLL);
    for _ in 0..200 {
        poll.tick().await;
        delivered = host.tool("events", JsonObject::default()).await?.text;
        if delivered.contains("settings.changed") {
            break;
        }
    }
    assert!(
        delivered.contains(r#""runs":4.0"#) || delivered.contains(r#""runs":4"#),
        "{delivered}"
    );
    Ok(())
}

async fn events_and_jobs_reach_the_process(host: &Harness) -> TestResult {
    let plugins = host.gateway.plugins().ok_or("no host")?;
    let session = SessionId::new(
        HarnessId::ClaudeCode,
        "00000000-0000-4000-8000-000000000001",
    );
    plugins.publish(&PluginEvent::SessionIdle {
        session_id: session.clone(),
    });
    plugins.publish(&PluginEvent::SessionEnded {
        session_id: session,
    });
    let mut delivered = String::new();
    let mut poll = tokio::time::interval(POLL);
    for _ in 0..200 {
        poll.tick().await;
        delivered = host.tool("events", JsonObject::default()).await?.text;
        if delivered.contains("session.idle") {
            break;
        }
    }
    assert!(delivered.contains("session.idle"), "{delivered}");
    assert!(
        !delivered.contains("session.ended"),
        "an unsubscribed kind was delivered: {delivered}"
    );
    let (status, _) = host
        .call(
            Method::POST,
            "/api/plugins/fixture/jobs/tick/run",
            None,
            false,
        )
        .await?;
    assert_eq!(status, StatusCode::ACCEPTED);
    host.until("a finished job", |record| {
        record.jobs.iter().any(|job| {
            job.last_run
                .as_ref()
                .is_some_and(|run| run.outcome == Some(PluginJobOutcome::Succeeded))
        })
    })
    .await?;
    Ok(())
}

async fn a_killed_process_is_restarted(host: &Harness) -> TestResult {
    let first: i32 = host
        .tool("pid", JsonObject::default())
        .await?
        .text
        .parse()?;
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(first),
        nix::sys::signal::Signal::SIGKILL,
    )?;
    let restarted = host
        .until("a restart", |record| {
            record.state == PluginState::Running && record.restarts == 1
        })
        .await?;
    assert_eq!(
        restarted.last_exit.and_then(|exit| exit.signal).as_deref(),
        Some("SIGKILL")
    );
    let second: i32 = host
        .tool("pid", JsonObject::default())
        .await?
        .text
        .parse()?;
    assert_ne!(first, second);
    let (_, bytes) = host
        .call(Method::GET, "/api/plugins/fixture/logs", None, false)
        .await?;
    let logs: PluginLogs = serde_json::from_slice(&bytes)?;
    assert!(
        logs.lines
            .iter()
            .any(|line| line.stream == PluginLogStream::Stdout
                && line.text.contains("/api/plugin/self answered 200 "))
    );
    assert!(
        logs.lines
            .iter()
            .any(|line| line.stream == PluginLogStream::Host && line.text == "ready")
    );
    Ok(())
}

async fn a_wider_manifest_waits_for_approval(host: &Harness, source: &Path) -> TestResult {
    write_manifest(
        source,
        &[
            Permission::SessionsRead,
            Permission::TopicsRead,
            Permission::AgentsRead,
        ],
    )?;
    let (status, bytes) = host
        .call(Method::POST, "/api/plugins/fixture/reload", None, false)
        .await?;
    assert_eq!(status, StatusCode::OK);
    let reloaded: PluginRecord = serde_json::from_slice(&bytes)?;
    assert_eq!(reloaded.state, PluginState::AwaitingApproval);
    assert_eq!(reloaded.pending, [Permission::AgentsRead]);
    let (status, _) = host
        .call(
            Method::POST,
            "/api/plugins/fixture/enable",
            enable(&[Permission::AgentsRead])?,
            true,
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    host.until("running again", |record| {
        record.state == PluginState::Running
    })
    .await?;
    let agents = host.probe_body("GET", "/api/agents").await?;
    assert!(agents.starts_with("200 "), "{agents}");
    Ok(())
}

async fn disable_and_remove(host: &Harness, root: &Path) -> TestResult {
    let (status, _) = host
        .call(Method::POST, "/api/plugins/fixture/disable", None, false)
        .await?;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = host
        .call(
            Method::POST,
            "/api/plugins/fixture/views/status",
            Some("{}".into()),
            false,
        )
        .await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = host
        .call(Method::DELETE, "/api/plugins/fixture", None, false)
        .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = host
        .call(Method::GET, "/api/plugins/fixture", None, false)
        .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!root.join("state/plugins/fixture").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_manifest_is_refused_with_its_field_path() -> TestResult {
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    std::fs::create_dir_all(&source)?;
    std::fs::write(source.join("rivethub-plugin.json"), invalid_manifest())?;
    let host = harness(root.path()).await?;
    let install = |path: &Path| -> Result<Option<String>, Failure> {
        Ok(Some(serde_json::to_string(&PluginInstallRequest {
            path: path.display().to_string(),
        })?))
    };
    let (status, bytes) = host
        .call(Method::POST, "/api/plugins", install(&source)?, false)
        .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(refused(&bytes)?.error.contains("colour"));
    let (status, _) = host
        .call(
            Method::POST,
            "/api/plugins",
            install(Path::new("relative"))?,
            false,
        )
        .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    host.gateway.close().await;
    Ok(())
}
