use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;

use config::provider::ProviderType;
use rivethub::composition::Composition;
use tokio_util::sync::CancellationToken;

use super::TestResult;

#[tokio::test]
async fn composing_the_gateway_starts_no_harness() -> TestResult {
    let home = tempfile::tempdir()?;
    let marker = home.path().join("harness-started");
    let harness = home.path().join("fake-harness");
    std::fs::write(
        &harness,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )?;
    std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o755))?;
    let text = format!(
        "runtime:\n  workspace: {home}\n  default_agent: main\nagents:\n  main:\n    provider: p\n    model: agent-model\nproviders:\n  p:\n    type: {kind}\n    model: provider-model\n    binary: {binary}\n    discovery_timeout_ms: 200\nmesh:\n  enabled: false\n  node_name: test-node\n",
        home = home.path().display(),
        kind = ProviderType::HermesCli,
        binary = harness.display(),
    );
    let path = home.path().join("config.yaml");
    std::fs::write(&path, text)?;
    let env = config::Environment::from_vars([
        (OsStr::new("HOME"), home.path().as_os_str()),
        (OsStr::new("PATH"), home.path().as_os_str()),
    ]);
    let config = config::Config::load(&env, &config::ConfigLocation::Current(path))?.config;
    let cancel = CancellationToken::new();
    let composition = Composition::build(
        &config,
        &env,
        Arc::new(testkit::FakeMemoryStore::default()),
        &cancel,
        &kernel::shutdown::Activities::new(),
    )
    .await?;
    assert!(
        !marker.exists(),
        "the gateway makes no model call, so it must not start the provider's harness"
    );
    cancel.cancel();
    composition.close().await?;
    Ok(())
}
