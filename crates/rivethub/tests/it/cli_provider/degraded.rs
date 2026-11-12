use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;
use std::time::Duration;

use rivethub::composition::Composition;
use rivethub::models::ModelFactory;
use tokio_util::sync::CancellationToken;
use wire::gateway::{DegradedProvider, HealthzResponse, ProviderRecovery};

use super::{TestResult, harness, recorded, turn};

fn configured(home: &std::path::Path) -> TestResult<(config::Config, config::Environment)> {
    let stalled = home.join("hermes");
    std::fs::write(&stalled, "#!/bin/sh\nexec sleep 600\n")?;
    std::fs::set_permissions(&stalled, std::fs::Permissions::from_mode(0o700))?;
    let env = config::Environment::from_vars([(OsStr::new("HOME"), home.as_os_str())]);
    let text = format!(
        "runtime:\n  workspace: {home}\n  default_agent: main\nagents:\n  main:\n    provider: p\n    model: agent-model\n  slow:\n    provider: q\n    model: slow-model\n  gone:\n    provider: r\n    model: gone-model\nproviders:\n  p:\n    type: claude-cli\n    model: provider-model\n  q:\n    type: hermes-cli\n    binary: {stalled}\n    discovery_timeout_ms: 200\n  r:\n    type: kimi-code\n    binary: {absent}\ntasks:\n  enabled: false\nmesh:\n  enabled: false\n  node_name: test-node\n",
        home = home.display(),
        stalled = stalled.display(),
        absent = home.join("absent").display(),
    );
    let path = home.join("config.yaml");
    std::fs::write(&path, text)?;
    Ok((
        config::Config::load(
            &env,
            &config::ConfigLocation::Current(path),
            config::Profile::Node,
        )?
        .config,
        env,
    ))
}

async fn refuses_a_turn_fast(
    factory: &ModelFactory,
    config: &config::Config,
    cancel: &CancellationToken,
) -> TestResult {
    let degraded = factory.build(config, "q", Some("slow-model"), cancel)?;
    let Err(refused) = tokio::time::timeout(
        Duration::from_secs(2),
        degraded.provider.chat(turn("slow-model")),
    )
    .await?
    else {
        return Err("a degraded provider answered a turn".into());
    };
    assert!(
        refused
            .to_string()
            .contains("provider q is degraded (retrying): ACP discovery timed out after 200ms"),
        "{refused}"
    );
    Ok(())
}

async fn reports_degraded_health(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    home: &std::path::Path,
) -> TestResult {
    let healthz: HealthzResponse = client
        .get(format!("http://{addr}/healthz"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(healthz.ok);
    assert_eq!(
        healthz.degraded_providers,
        [
            DegradedProvider {
                id: "q".into(),
                recovery: ProviderRecovery::Retrying,
                reason: "ACP discovery timed out after 200ms".into(),
            },
            DegradedProvider {
                id: "r".into(),
                recovery: ProviderRecovery::Permanent,
                reason: format!(
                    "the ACP invocation of {} for kimi-code is not verified: not installed",
                    home.join("absent").display()
                ),
            },
        ]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_harness_that_fails_discovery_degrades_its_provider_not_the_gateway() -> TestResult {
    let home = tempfile::tempdir()?;
    let (config, env) = configured(home.path())?;
    let (events, expected) = recorded()?;
    let cancel = CancellationToken::new();
    let factory = tokio::time::timeout(
        Duration::from_secs(10),
        ModelFactory::default()
            .with_harness("p".into(), Arc::new(harness::Harness::new(events)))
            .with_agent_providers(&config, &env, &cancel),
    )
    .await??;
    refuses_a_turn_fast(&factory, &config, &cancel).await?;
    let composition = Composition::with_models(
        &config,
        &env,
        Arc::new(testkit::FakeMemoryStore::default()),
        &cancel,
        &factory,
    )
    .await?;
    let server = testkit::stub_server(composition.router(false).layer(axum::Extension(
        axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 12345))),
    )))
    .await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .default_headers(crate::owner::headers(&gateway::state_dir(&env)?)?)
        .build()?;
    reports_degraded_health(&client, server.addr, home.path()).await?;
    let reply = factory
        .build(&config, "p", Some("agent-model"), &cancel)?
        .provider
        .chat(turn("agent-model"))
        .await?;
    assert_eq!(reply.content.as_deref(), Some(expected.as_str()));
    server.close().await?;
    tokio::time::timeout(Duration::from_secs(5), composition.close()).await??;
    Ok(())
}
