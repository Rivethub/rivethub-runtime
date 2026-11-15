use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

mod common;

#[path = "common/aliases.rs"]
mod aliases;

#[derive(Deserialize)]
struct FormatCase {
    #[serde(rename = "in")]
    input: FormatInput,
    out: String,
}

#[derive(Deserialize, serde::Serialize)]
struct FormatInput {
    #[serde(default)]
    errors: Vec<config::Finding>,
    #[serde(default)]
    warnings: Vec<config::Finding>,
}

#[test]
fn format_validation_result_replay() -> Result<(), Box<dyn std::error::Error>> {
    let lines = common::read_cases("format_validation_result")?;
    let mut failures = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let case: FormatCase = common::parse_case("format_validation_result", index, line)?;
        let mut result = config::ValidationResult::default();
        for finding in &case.input.errors {
            result.push(finding.clone());
        }
        for finding in &case.input.warnings {
            result.push(finding.clone());
        }
        let actual = result.to_string();
        if actual != case.out {
            failures.push(common::case_failure(
                &common::case_id("format_validation_result", index),
                "formatted result diverges",
                &case.input,
                &case.out,
                &actual,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{}/{} format_validation_result.jsonl cases diverge:\n\n{}",
        failures.len(),
        lines.len(),
        failures.join("\n\n")
    );
    Ok(())
}

#[derive(Deserialize)]
struct PathsCase {
    #[serde(rename = "in")]
    input: PathsInput,
    out: String,
}

#[derive(Deserialize, serde::Serialize)]
struct PathsInput {
    env: BTreeMap<String, String>,
    #[serde(default)]
    segments: Vec<String>,
}

fn replay_paths(
    name: &str,
    apply: fn(&config::Environment, &[String]) -> PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let lines = common::read_cases(name)?;
    let mut failures = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let case: PathsCase = common::parse_case(name, index, line)?;
        let env = config::Environment::from_vars(case.input.env.clone());
        aliases::assert_deprecated_aliases(&env);
        let actual = apply(&env, &case.input.segments).display().to_string();
        if actual != case.out {
            failures.push(common::case_failure(
                &common::case_id(name, index),
                "resolved path diverges",
                &case.input,
                &case.out,
                &actual,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{}/{} {name}.jsonl cases diverge:\n\n{}",
        failures.len(),
        lines.len(),
        failures.join("\n\n")
    );
    Ok(())
}

fn shared_dir(env: &config::Environment, _segments: &[String]) -> PathBuf {
    config::shared_dir(env)
}

#[test]
fn shared_dir_replay() -> Result<(), Box<dyn std::error::Error>> {
    replay_paths("shared_dir", shared_dir)
}
