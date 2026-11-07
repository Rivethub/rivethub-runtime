use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::Deserialize;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Deserialize)]
struct Recording {
    runs: BTreeMap<String, Refusal>,
}

#[derive(Deserialize)]
struct Refusal {
    env: Settings,
    error: String,
}

#[derive(Deserialize)]
struct Settings {
    host: String,
    tls_cert: Option<String>,
    tls_key: Option<String>,
}

#[derive(Deserialize)]
struct Skip {
    scenario: String,
    reason: String,
}

fn recording() -> Result<Recording, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&std::fs::read(
        testkit::recording("gateway/startup.json"),
    )?)?)
}

fn skips() -> Result<Vec<Skip>, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&std::fs::read(
        testkit::recording("gateway/composition-skips.json"),
    )?)?)
}

#[tokio::test]
async fn binary_replays_recorded_startup_refusals() -> TestResult {
    let skipped: BTreeSet<_> = skips()?.into_iter().map(|skip| skip.scenario).collect();
    for (name, refusal) in recording()?.runs {
        if skipped.contains(&name) {
            continue;
        }
        let home = tempfile::tempdir()?;
        let state = home.path().join(".rivethub");
        std::fs::create_dir_all(home.path().join("pki"))?;
        std::fs::create_dir(&state)?;
        std::fs::write(home.path().join("pki/node.crt"), "cert")?;
        std::fs::write(home.path().join("pki/node.key"), "key")?;
        let replace = |text: &str| text.replace("{{TMP}}", &home.path().to_string_lossy());
        let mut config = format!(
            "runtime:\n  workspace: {}\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    model: m\ngateway:\n  host: {}\n",
            home.path().display(),
            refusal.env.host
        );
        for (key, value) in [
            ("tls_cert", refusal.env.tls_cert),
            ("tls_key", refusal.env.tls_key),
        ] {
            if let Some(value) = value {
                use std::fmt::Write as _;
                writeln!(config, "  {key}: {}", replace(&value))?;
            }
        }
        std::fs::write(state.join("config.yaml"), config)?;
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_rh"))
                .args(["gateway", "--check"])
                .env_clear()
                .env("HOME", home.path())
                .env("RIVETHUB_SHARED_DIR", home.path())
                .env("TOKIO_WORKER_THREADS", "2")
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        assert_eq!(output.status.code(), Some(3), "{name}");
        let stderr = String::from_utf8(output.stderr)?;
        let actual = stderr
            .lines()
            .find_map(|line| {
                line.split_once("[rivethub-gateway]")
                    .map(|(_, message)| format!("[rivethub-gateway]{message}"))
            })
            .ok_or_else(|| format!("missing refusal for {name}: {stderr}"))?;
        assert_eq!(
            actual,
            testkit::naming::translate_recorded_product_names(&replace(&refusal.error)),
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn every_recorded_startup_scenario_is_replayed_or_has_a_named_reason() -> TestResult {
    let recording = recording()?;
    let skipped = skips()?;
    let mut inventory = BTreeSet::from(["refuse-offloopback-no-tls".to_owned()]);
    for skip in skipped {
        assert_ne!(skip.reason.trim(), "");
        assert!(inventory.insert(skip.scenario));
    }
    assert_eq!(inventory, recording.runs.into_keys().collect());
    Ok(())
}
