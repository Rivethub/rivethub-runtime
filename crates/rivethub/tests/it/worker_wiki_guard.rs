use std::path::Path;
use std::time::Duration;

use crate::process::{DEAD_POSTGRES, TestResult, provider_config, write_state};

async fn check(home: &Path, role: &str, extraction: Option<&str>) -> TestResult<(bool, String)> {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_rh"));
    command
        .args(["worker", "--role", role, "--check"])
        .env_clear()
        .env("HOME", home);
    if let Some(value) = extraction {
        command.env("RIVETHUB_WIKI_EXTRACTION", value);
    }
    let output = tokio::time::timeout(Duration::from_secs(30), command.output()).await??;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

#[tokio::test]
async fn wiki_roles_refuse_to_start_without_wiki_extraction() -> TestResult {
    let home = tempfile::tempdir()?;
    write_state(
        home.path(),
        &provider_config(home.path(), DEAD_POSTGRES, ""),
    )?;
    for role in ["wiki-builder", "all"] {
        for extraction in [None, Some("0")] {
            let (ok, text) = check(home.path(), role, extraction).await?;
            assert!(!ok, "{role} {extraction:?}: {text}");
            assert!(
                text.contains("set RIVETHUB_WIKI_EXTRACTION=1"),
                "{role}: {text}"
            );
        }
        let (ok, text) = check(home.path(), role, Some("1")).await?;
        assert!(ok, "{role}: {text}");
        assert!(text.contains("worker configuration valid"), "{text}");
    }
    let (ok, text) = check(home.path(), "summarizer", None).await?;
    assert!(ok, "summarizer does not claim extract-wiki: {text}");
    Ok(())
}
