use chrono::DateTime;
use memory_sqlite::SqliteStore;
use serde::Deserialize;

#[derive(Deserialize)]
struct Recording {
    scenarios: Vec<Scenario>,
}

#[derive(Deserialize)]
struct Scenario {
    name: String,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Step {
    label: String,
    result: StepResult,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StepResult {
    Health(HealthSnapshot),
    Count(u64),
}

#[derive(Deserialize)]
struct HealthSnapshot {
    #[serde(rename = "compaction")]
    summarizer: Option<SummarizerCounts>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SummarizerCounts {
    eligible: u64,
    active_tail: u64,
    below_floor: u64,
}

#[tokio::test]
async fn empty_store_summarizer_health_keeps_the_recorded_shape()
-> Result<(), Box<dyn std::error::Error>> {
    let recording: Recording = serde_json::from_str(&std::fs::read_to_string(
        testkit::recording("memory-sqlite/stats-health.json"),
    )?)?;
    let expected = recording
        .scenarios
        .iter()
        .find(|scenario| scenario.name == "empty store")
        .and_then(|scenario| scenario.steps.iter().find(|step| step.label == "health"))
        .and_then(|step| match &step.result {
            StepResult::Health(health) => health.summarizer.as_ref(),
            StepResult::Count(_completed_jobs) => None,
        })
        .ok_or("empty-store summarizer health recording")?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let store = SqliteStore::open(&path)?;
    let now = DateTime::parse_from_rfc3339("2026-10-10T12:00:00Z")?.to_utc();
    let health = store.health(now).await?;
    assert_eq!(health.summarizer.eligible, expected.eligible);
    assert_eq!(health.summarizer.active_tail, expected.active_tail);
    assert_eq!(health.summarizer.below_floor, expected.below_floor);
    Ok(())
}
