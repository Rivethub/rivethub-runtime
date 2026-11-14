mod documentation;

use documentation::deviations_are_documented;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use config::{Finding, PLUGINS_IGNORED, ProviderType, RemovedProviderType, WORKFLOWS_IGNORED};
use libtest_mimic::{Arguments, Failed, Trial};
use std::str::FromStr;

#[path = "../common/mod.rs"]
mod common;

const GOLDEN_FILES: [&str; 8] = [
    "validate_config",
    "validate_config_runtime",
    "validate_config_agents_providers",
    "validate_config_memory",
    "validate_config_den",
    "validate_config_tasks_workflows",
    "validate_config_mesh_channels_misc",
    "validate_config_untyped",
];

#[derive(serde::Deserialize)]
struct Case {
    #[serde(rename = "in")]
    input: serde_yaml_ng::Value,
    out: Recorded,
}

#[derive(serde::Deserialize)]
struct Recorded {
    #[serde(default)]
    errors: Vec<Finding>,
    #[serde(default)]
    warnings: Vec<Finding>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Deviation {
    id: String,
    class: DeviationClass,
    #[serde(default)]
    findings: Vec<Finding>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, strum::Display,
)]
enum DeviationClass {
    #[serde(rename = "rejected")]
    #[strum(serialize = "accepted-but-rejected")]
    Rejected,
    #[serde(rename = "order")]
    #[strum(serialize = "order-only Available lists")]
    Order,
    #[serde(rename = "seq_form")]
    #[strum(serialize = "sequence-shaped sections or entries")]
    SeqForm,
    #[serde(rename = "null_missing")]
    #[strum(serialize = "explicit nulls read as missing")]
    NullMissing,
}

struct Replay {
    id: String,
    input: serde_yaml_ng::Value,
    recorded_accepted: bool,
    contract: Vec<Finding>,
    deviation: Option<Deviation>,
    removed: RemovedDeltas,
}

fn golden_path(subpath: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(subpath)
}

fn read_cases(name: &str) -> Result<Vec<Case>, String> {
    let lines = common::read_cases(name).map_err(|error| error.to_string())?;
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            common::parse_case(name, index, line).map_err(|error| error.to_string())
        })
        .collect()
}

fn load_deviations() -> Result<Vec<Deviation>, String> {
    let path = golden_path("deviations.jsonl");
    let text = fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|error| format!("{}: {error}", path.display()))
        })
        .collect()
}

fn section_value<'a>(
    input: &'a serde_yaml_ng::Value,
    key: &str,
) -> Option<&'a serde_yaml_ng::Value> {
    if input.is_mapping() {
        input.get(key)
    } else {
        None
    }
}

fn removed_provider_ids(input: &serde_yaml_ng::Value) -> Vec<(String, RemovedProviderType)> {
    let Some(providers) =
        section_value(input, "providers").and_then(|providers| providers.as_mapping())
    else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for (id, entry) in providers {
        let Some(id) = id.as_str() else {
            continue;
        };
        let declared = entry.get("type").and_then(serde_yaml_ng::Value::as_str);
        let kind = match declared {
            Some(text) => text.parse::<RemovedProviderType>().ok(),
            None => id.parse::<RemovedProviderType>().ok(),
        };
        if let Some(kind) = kind {
            removed.push((id.to_owned(), kind));
        }
    }
    removed
}

const DECISION_15_WARNING: &str = "memory.capture.redaction is validated but not yet injected into harness hook processes; set RIVETOS_CAPTURE_REDACTION or pass CaptureWriterOptions.redaction to enable at runtime";

const EXPOSED_TERMINAL: &str = "an exposed terminal without TLS";

const REMOVED_BY_DECISION_ROWS: usize = 34_398;
const REMOVED_BY_DECISION_CASES: usize = 5_823;

fn decision_replacements(input: &serde_yaml_ng::Value) -> Vec<(String, bool, String, String)> {
    let mut replacements = Vec::new();
    if section_value(input, "plugins").is_some() {
        replacements.push((
            "plugins".to_owned(),
            false,
            "plugins".to_owned(),
            PLUGINS_IGNORED.to_owned(),
        ));
    }
    if section_value(input, "workflows").is_some() {
        replacements.push((
            "workflows".to_owned(),
            false,
            "workflows".to_owned(),
            WORKFLOWS_IGNORED.to_owned(),
        ));
    }
    for (id, kind) in removed_provider_ids(input) {
        replacements.push((
            format!("providers.{id}"),
            true,
            format!("providers.{id}.type"),
            format!(
                "Provider \"{id}\" type \"{kind}\" is not built — {}.",
                kind.message()
            ),
        ));
    }
    replacements
}

fn covered(path: &str, section: &str) -> bool {
    path == section
        || path.starts_with(&format!("{section}."))
        || path.starts_with(&format!("{section}["))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString)]
#[strum(serialize_all = "snake_case")]
enum RemovedProviderKey {
    Temperature,
    TopP,
    TopK,
    MinP,
    PresencePenalty,
    FrequencyPenalty,
    Seed,
    Stop,
    RepetitionPenalty,
    MinTokens,
    DefaultToolChoice,
    MmProcessorKwargs,
    ChatTemplateKwargs,
    ExtraBody,
    Models,
    ModelsTtlMs,
    ModelsUrl,
    VerifyModelOnInit,
    NumCtx,
    KeepAlive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString)]
enum RemovedEmbedKey {
    #[strum(serialize = "embed_token_command")]
    TokenCommand,
    #[strum(serialize = "embed_token_ttl_ms")]
    TokenTtlMs,
    #[strum(serialize = "embed_token_command_timeout_ms")]
    TokenCommandTimeoutMs,
    #[strum(serialize = "embed_wire_shape")]
    WireShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
enum RemovedByDecision {
    ProviderKey(RemovedProviderKey),
    EmbedKey(RemovedEmbedKey),
    GoneAutoMigrate,
    RetiredTaskKey,
    RetiredSection,
}

fn key_warning(path: String, key: &str) -> Finding {
    Finding::warning(
        path,
        format!("{key} is set on the model server, not in RivetHub"),
    )
}

#[derive(Debug, Default)]
struct RemovedDeltas {
    paths: Vec<String>,
    findings: Vec<(RemovedByDecision, Finding)>,
}

impl RemovedDeltas {
    fn push(&mut self, path: &str, reason: RemovedByDecision, finding: Finding) {
        self.paths.push(path.to_owned());
        self.findings.push((reason, finding));
    }
}

fn provider_key_deltas(input: &serde_yaml_ng::Value, deltas: &mut RemovedDeltas) {
    let Some(providers) =
        section_value(input, "providers").and_then(|providers| providers.as_mapping())
    else {
        return;
    };
    for (id, entry) in providers {
        let Some(id) = id.as_str() else {
            continue;
        };
        let Some(entry) = entry.as_mapping() else {
            continue;
        };
        let declared = entry.get("type").and_then(serde_yaml_ng::Value::as_str);
        let name = declared.unwrap_or(id);
        if name.parse::<ProviderType>().is_err() {
            continue;
        }
        for (key, _) in entry {
            let Some(key) = key.as_str() else {
                continue;
            };
            let Ok(removed) = RemovedProviderKey::from_str(key) else {
                continue;
            };
            deltas.push(
                &format!("providers.{id}.{key}"),
                RemovedByDecision::ProviderKey(removed),
                key_warning(format!("providers.{id}.{key}"), &removed.to_string()),
            );
        }
    }
}

fn embed_key_deltas(input: &serde_yaml_ng::Value, deltas: &mut RemovedDeltas) {
    let Some(memory) = section_value(input, "memory").and_then(|memory| memory.as_mapping()) else {
        return;
    };
    let mut embed_keys = |entries: &serde_yaml_ng::Value, prefix: &str| {
        let Some(entries) = entries.as_mapping() else {
            return;
        };
        for (key, _) in entries {
            let Some(key) = key.as_str() else {
                continue;
            };
            let Ok(removed) = RemovedEmbedKey::from_str(key) else {
                continue;
            };
            deltas.push(
                &format!("{prefix}.{key}"),
                RemovedByDecision::EmbedKey(removed),
                key_warning(format!("{prefix}.{key}"), &removed.to_string()),
            );
        }
    };
    for section in ["postgres", "sqlite"] {
        if let Some(entries) = memory.get(section) {
            embed_keys(entries, &format!("memory.{section}"));
        }
    }
    if let Some(embedded) = memory
        .get("postgres")
        .and_then(|postgres| postgres.get("embedded"))
    {
        embed_keys(embedded, "memory.postgres.embedded");
    }
}

fn auto_migrate_deltas(input: &serde_yaml_ng::Value, deltas: &mut RemovedDeltas) {
    let Some(embedded) = section_value(input, "memory")
        .and_then(serde_yaml_ng::Value::as_mapping)
        .and_then(|memory| memory.get("postgres"))
        .and_then(serde_yaml_ng::Value::as_mapping)
        .and_then(|postgres| postgres.get("embedded"))
        .and_then(serde_yaml_ng::Value::as_mapping)
    else {
        return;
    };
    if embedded.get("auto_migrate").is_some() {
        deltas.push(
            "memory.postgres.embedded.auto_migrate",
            RemovedByDecision::GoneAutoMigrate,
            Finding::warning(
                "memory.postgres.embedded.auto_migrate",
                "auto_migrate is gone; run rivethub-migrate",
            ),
        );
    }
}

fn retired_task_deltas(input: &serde_yaml_ng::Value, deltas: &mut RemovedDeltas) {
    for (section, key, why) in [
        ("tasks", "pricing", "maxUsd budgets were removed"),
        ("tasks", "eval", "task evals were removed"),
        (
            "runtime",
            "heartbeats",
            "scheduled heartbeat tasks were removed",
        ),
    ] {
        let present = section_value(input, section)
            .and_then(serde_yaml_ng::Value::as_mapping)
            .is_some_and(|mapping| mapping.get(key).is_some());
        if present {
            let path = format!("{section}.{key}");
            deltas.push(
                &path,
                RemovedByDecision::RetiredTaskKey,
                Finding::warning(
                    path.clone(),
                    format!("\"{path}\" is no longer used: {why}. Delete it."),
                ),
            );
        }
    }
}

fn dotted<'a>(input: &'a serde_yaml_ng::Value, path: &str) -> Option<&'a serde_yaml_ng::Value> {
    let mut parts = path.split('.');
    let head = parts.next()?;
    let mut value = match section_value(input, head) {
        Some(value) => value,
        None if head == "gateway" => section_value(input, "den")?,
        None => return None,
    };
    for part in parts {
        value = value.as_mapping()?.get(part)?;
    }
    Some(value)
}

fn retired_section_deltas(input: &serde_yaml_ng::Value, deltas: &mut RemovedDeltas) {
    for retired in config::validate::retired::RETIRED {
        let Some(value) = dotted(input, retired.path)
            .filter(|value| retired.keeps.is_empty() || !value.is_null())
        else {
            continue;
        };
        let used = retired.keeps.is_empty()
            || value.as_mapping().is_none_or(|mapping| {
                mapping
                    .keys()
                    .any(|key| key.as_str().is_none_or(|key| !retired.keeps.contains(&key)))
            });
        if used {
            deltas.push(
                retired.path,
                RemovedByDecision::RetiredSection,
                Finding::warning(retired.path, retired.message()),
            );
        }
    }
}

fn removed_by_decision(input: &serde_yaml_ng::Value) -> RemovedDeltas {
    let mut deltas = RemovedDeltas::default();
    provider_key_deltas(input, &mut deltas);
    embed_key_deltas(input, &mut deltas);
    auto_migrate_deltas(input, &mut deltas);
    retired_task_deltas(input, &mut deltas);
    retired_section_deltas(input, &mut deltas);
    deltas
}

fn optional_section(finding: &Finding) -> bool {
    matches!(finding.path.as_str(), "runtime" | "agents" | "providers")
        && finding.message.starts_with("Missing required section")
}

fn under(path: &str, removed: &str) -> bool {
    path.strip_prefix(removed)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.') || rest.starts_with('['))
}

fn gateway_finding(mut finding: Finding) -> Finding {
    let Some(rest) = finding.path.strip_prefix("den") else {
        return finding;
    };
    if !(rest.is_empty() || rest.starts_with('.') || rest.starts_with('[')) {
        return finding;
    }
    finding.path = format!("gateway{rest}");
    finding.message = finding
        .message
        .replace("\"den\" must", "\"gateway\" must")
        .replace("Unknown den key", "Unknown gateway key")
        .replace("den.", "gateway.");
    finding
}

fn contract_findings(input: &serde_yaml_ng::Value, recorded: &Recorded) -> Vec<Finding> {
    let replacements = decision_replacements(input);
    let mut findings: Vec<Finding> = recorded
        .errors
        .iter()
        .chain(&recorded.warnings)
        .filter(|finding| finding.message != DECISION_15_WARNING)
        .filter(|finding| {
            !replacements
                .iter()
                .any(|(scope, _, _, _)| covered(&finding.path, scope))
        })
        .cloned()
        .map(gateway_finding)
        .collect();
    for (_, error, path, message) in replacements {
        findings.push(Finding {
            severity: if error {
                config::Severity::Error
            } else {
                config::Severity::Warning
            },
            path,
            message,
        });
    }
    findings
}

fn multiset(findings: &[Finding]) -> BTreeSet<(String, String, String)> {
    findings
        .iter()
        .map(|finding| {
            (
                finding.severity.to_string(),
                finding.path.clone(),
                finding.message.clone(),
            )
        })
        .collect()
}

fn sorted_findings(findings: &[Finding]) -> Vec<Finding> {
    let mut sorted = findings.to_vec();
    sorted.sort_by_key(|finding| {
        (
            finding.severity.to_string(),
            finding.path.clone(),
            finding.message.clone(),
        )
    });
    sorted
}

fn outcome_failure(case: &Replay, reason: &str, expected: &str, actual: &str) -> String {
    common::case_failure(&case.id, reason, &case.input, expected, actual)
}

fn replay_type_confused(case: &Replay, actual: &config::ValidationResult) -> Result<(), String> {
    if actual.errors.len() != 1 {
        return Err(outcome_failure(
            case,
            "a type-confused document must produce exactly one wrong-type error",
            "exactly one error finding at the serde path",
            &format!(
                "{} error findings: {:?}",
                actual.errors.len(),
                multiset(&actual.errors)
            ),
        ));
    }
    match case.deviation.as_ref().map(|deviation| deviation.class) {
        Some(DeviationClass::Rejected) if case.recorded_accepted => Ok(()),
        Some(DeviationClass::Rejected) => Err(outcome_failure(
            case,
            "the deviations table pins accepted-but-rejected, but the recording rejects this input",
            "the recording accepts this input",
            &format!("the recording rejects it: {:?}", multiset(&actual.errors)),
        )),
        Some(class) => Err(outcome_failure(
            case,
            "the document is type-confused but the deviations table pins another class",
            "class accepted-but-rejected (or no deviations row)",
            &format!("class {class}"),
        )),
        None if case.recorded_accepted => Err(outcome_failure(
            case,
            "accepted by the recording but rejected here, and absent from the deviations table",
            "accepted (the recording records no errors)",
            &format!("rejected: {:?}", multiset(&actual.errors)),
        )),
        None => Ok(()),
    }
}

fn replay_well_typed(
    case: &Replay,
    expected: &[Finding],
    actual: &[Finding],
) -> Result<(), String> {
    if multiset(expected) == multiset(actual) {
        return Ok(());
    }
    Err(common::case_failure(
        &case.id,
        "findings diverge (compared as a multiset of severity, path, message)",
        &case.input,
        &common::json_text(&sorted_findings(expected)),
        &common::json_text(&sorted_findings(actual)),
    ))
}

fn replay_case(case: &Replay) -> Result<(), String> {
    let actual = config::validate_document(&case.input);
    if config::type_document(&case.input).is_err() {
        return replay_type_confused(case, &actual);
    }
    let mut expected_findings: Vec<Finding> = match &case.deviation {
        Some(Deviation {
            class: DeviationClass::Rejected,
            ..
        }) => {
            return Err(outcome_failure(
                case,
                "the deviations table pins accepted-but-rejected, but the document is well-typed",
                "a type-confused document",
                "a well-typed document",
            ));
        }
        Some(deviation) => deviation
            .findings
            .iter()
            .cloned()
            .map(gateway_finding)
            .collect(),
        None => case.contract.clone(),
    };
    let terminal_retired = case
        .removed
        .paths
        .iter()
        .any(|removed| removed == "gateway.terminal");
    expected_findings.retain(|finding| {
        let retired = case
            .removed
            .paths
            .iter()
            .any(|removed| under(&finding.path, removed));
        let exposed = terminal_retired && finding.message.contains(EXPOSED_TERMINAL);
        !(retired || exposed || optional_section(finding))
    });
    expected_findings.extend(
        case.removed
            .findings
            .iter()
            .map(|(_, finding)| finding.clone()),
    );
    for finding in &mut expected_findings {
        if finding.path == "memory.postgres.review_endpoint" {
            finding.message = finding
                .message
                .replace("compaction is", "the summarizer is");
        }
    }
    let actual_findings: Vec<Finding> = actual
        .errors
        .iter()
        .chain(&actual.warnings)
        .cloned()
        .collect();
    replay_well_typed(case, &expected_findings, &actual_findings)
}

fn replay_file(name: &str, deviations: &BTreeMap<String, Deviation>) -> Result<(), Failed> {
    let cases = read_cases(name).map_err(Failed::from)?;
    let mut failures = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let id = common::case_id(name, index);
        let replay = Replay {
            id: id.clone(),
            input: case.input.clone(),
            recorded_accepted: case.out.errors.is_empty(),
            contract: contract_findings(&case.input, &case.out),
            deviation: deviations.get(&id).cloned(),
            removed: removed_by_decision(&case.input),
        };
        if let Err(failure) = replay_case(&replay) {
            failures.push(failure);
        }
    }
    if failures.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}/{} cases in {name}.jsonl diverge:\n\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n\n")
    )
    .into())
}

fn removed_by_decision_table_size() -> Result<(usize, usize), String> {
    let mut rows = 0usize;
    let mut cases = 0usize;
    for name in GOLDEN_FILES {
        for case in read_cases(name)? {
            let deltas = removed_by_decision(&case.input);
            if deltas.findings.is_empty() {
                continue;
            }
            cases += 1;
            rows += deltas.findings.len();
        }
    }
    Ok((rows, cases))
}

fn collect_trials() -> Result<Vec<Trial>, String> {
    let deviations: BTreeMap<String, Deviation> = load_deviations()?
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect();
    let mut trials = Vec::new();
    for name in GOLDEN_FILES {
        let file_deviations = deviations.clone();
        trials.push(Trial::test(format!("{name}.jsonl"), move || {
            replay_file(name, &file_deviations)
        }));
    }
    trials.push(Trial::test(
        "deviations_are_documented".to_owned(),
        deviations_are_documented,
    ));
    trials.push(Trial::test("removed_by_decision_table".to_owned(), || {
        let (rows, cases) = removed_by_decision_table_size()?;
        if rows != REMOVED_BY_DECISION_ROWS || cases != REMOVED_BY_DECISION_CASES {
            return Err(format!(
                "the RemovedByDecision table drifted: pinned {REMOVED_BY_DECISION_ROWS} rows over {REMOVED_BY_DECISION_CASES} cases, table holds {rows} rows over {cases} cases"
            )
            .into());
        }
        Ok(())
    }));
    Ok(trials)
}

fn main() {
    let arguments = Arguments::from_args();
    let trials = match collect_trials() {
        Ok(trials) => trials,
        Err(error) => vec![Trial::test("collect_cases", move || Err(error.into()))],
    };
    libtest_mimic::run(&arguments, trials).exit();
}
