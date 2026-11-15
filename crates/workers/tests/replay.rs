#![cfg(test)]

#[expect(unreachable_pub)]
#[expect(dead_code)]
mod common;

use std::str::FromStr;

use common::Store;
use common::embed_target;
use common::settings;
use common::width;
use kernel::memory::EmbeddingStore;
use kernel::queue::FixedJob;
use kernel::queue::JobName;
use serde::Deserialize;
use testkit::FakeProvider;
use testkit::Json;
use testkit::capture_pg::Recording;
use testkit::capture_pg::ScenarioRecord;
use testkit::capture_pg::StepRecord;
use tokio_util::sync::CancellationToken;
use workers::EmbedTable;
use workers::EmbedTarget;
use workers::LoadedTarget;
use workers::StoredEmbedding;
use workers::classify_unembeddable;
use workers::compose_message;
use workers::content_hash;
use workers::embed_key;

const FILES: [&str; 5] = [
    "capture/pg/transcript.json",
    "capture/pg/task.json",
    "capture/pg/routed.json",
    "capture/pg/hook.json",
    "capture/pg/errors.json",
];

const SKIPPED: [&str; 3] = [
    "routed-without-0013",
    "routed-half-0013",
    "task-id-absent-column",
];

#[test]
fn recorded_scope_is_partitioned() {
    let records = recordings().expect("recordings");
    let mut replayed = 0_usize;
    let mut skipped_names = Vec::new();
    for scenario in records
        .iter()
        .flat_map(|recording| recording.scenarios.iter())
    {
        if skipped(&scenario.name) {
            assert_graphile_absent(scenario).expect("skipped scenario");
            skipped_names.push(scenario.name.clone());
        } else {
            assert_graphile_present(scenario).expect("replayed scenario");
            replayed += 1;
        }
    }
    assert_eq!(replayed, 27);
    assert_eq!(skipped_names.len(), SKIPPED.len());
    assert_eq!(replayed + skipped_names.len(), 30);
    for name in SKIPPED {
        assert!(skipped_names.iter().any(|seen| seen == name), "{name}");
    }
}

#[tokio::test]
async fn recorded_embed_jobs_replay_against_their_messages() {
    let records = recordings().expect("recordings");
    for scenario in records
        .iter()
        .flat_map(|recording| recording.scenarios.iter())
    {
        if skipped(&scenario.name) {
            continue;
        }
        for step in &scenario.steps {
            replay_step(&scenario.name, step).await.expect("replay");
        }
    }
}

fn recordings() -> Result<Vec<Recording>, String> {
    FILES
        .iter()
        .map(|name| {
            let text = std::fs::read_to_string(testkit::recording(name))
                .map_err(|error| format!("read {name}: {error}"))?;
            serde_json::from_str(&text).map_err(|error| format!("parse {name}: {error}"))
        })
        .collect()
}

fn skipped(name: &str) -> bool {
    SKIPPED.contains(&name)
}

fn assert_graphile_absent(scenario: &ScenarioRecord) -> Result<(), String> {
    for step in &scenario.steps {
        let rows = step_rows(step)?;
        if has_key(rows, "graphile_jobs") {
            return Err(format!(
                "{} still has graphile_jobs on schema {}",
                scenario.name, scenario.schema
            ));
        }
    }
    Ok(())
}

fn assert_graphile_present(scenario: &ScenarioRecord) -> Result<(), String> {
    for step in &scenario.steps {
        let rows = step_rows(step)?;
        if !has_key(rows, "graphile_jobs") {
            return Err(format!("{} is missing graphile_jobs", scenario.name));
        }
        array(rows, "graphile_jobs")?;
    }
    Ok(())
}

async fn replay_step(scenario: &str, step: &StepRecord) -> Result<(), String> {
    let rows = step_rows(step)?;
    let jobs = array(rows, "graphile_jobs")?;
    let store = seed_messages(array(rows, "ros_messages")?).await?;
    let mut embedded = 0_usize;
    let mut marked = 0_usize;
    for job in jobs {
        match replay_job(scenario, &store, job).await? {
            ReplayHit::Embedded => embedded += 1,
            ReplayHit::Marked => marked += 1,
        }
    }
    if store.writes().await != embedded || embedded + marked != jobs.len() {
        return Err(format!(
            "{scenario} wrote {} embeddings and marked {marked} unembeddable for {} jobs",
            store.writes().await,
            jobs.len()
        ));
    }
    Ok(())
}

enum ReplayHit {
    Embedded,
    Marked,
}

async fn seed_messages(messages: &[Json]) -> Result<Store, String> {
    let store = Store::new();
    for message in messages {
        let row: RecordedMessage =
            serde_json::from_value(message.clone()).map_err(|error| format!("message: {error}"))?;
        #[expect(clippy::manual_unwrap_or_default)]
        let content = match row.content.as_deref() {
            Some(text) => text,
            None => "",
        };
        store
            .insert(
                EmbedTable::Messages,
                &row.id,
                content,
                row.tool_result.as_deref(),
            )
            .await?;
    }
    Ok(store)
}

async fn replay_job(scenario: &str, store: &Store, value: &Json) -> Result<ReplayHit, String> {
    let job: RecordedJob =
        serde_json::from_value(value.clone()).map_err(|error| format!("{scenario}: {error}"))?;
    let name = JobName::from_str(&job.task).map_err(|error| error.to_string())?;
    if !matches!(name, JobName::Fixed(FixedJob::EmbedTarget)) {
        return Err(format!("{scenario} queued {}", job.task));
    }
    if job.priority != 0 || job.max_attempts != 5 || job.attempts != 0 || job.queue.is_some() {
        return Err(format!(
            "{scenario} job {} does not match the embed contract",
            job.key
        ));
    }
    let fields: TargetFields =
        serde_json::from_str(&job.payload).map_err(|error| format!("{scenario}: {error}"))?;
    let table = EmbedTable::from_str(&fields.target_table)
        .map_err(|_| format!("{scenario} table {}", fields.target_table))?;
    let target = EmbedTarget {
        table,
        id: fields.target_id,
    };
    if job.key != embed_key(&target) {
        return Err(format!(
            "{scenario} key {} != {}",
            job.key,
            embed_key(&target)
        ));
    }
    let provider = FakeProvider::fnv_embeddings(width());
    let payload = common::target_payload(target.table.to_string().as_str(), &target.id);
    embed_target(
        store,
        &provider,
        settings(),
        &payload,
        &CancellationToken::new(),
    )
    .await
    .map_err(|error| format!("{scenario} {}: {error}", target.id))?;
    if let Some(reason) = store.unembeddable(target.table, &target.id).await {
        if store.written(target.table, &target.id).await.is_some() {
            return Err(format!(
                "{scenario} {} marked unembeddable and stored a vector",
                target.id
            ));
        }
        if !provider.embed_requests().await.is_empty() {
            return Err(format!(
                "{scenario} {} marked unembeddable after an embed call",
                target.id
            ));
        }
        let loaded = load_target(scenario, store, &target).await?;
        let cap = usize::try_from(settings().tool_result_cap).expect("cap");
        let composed = compose_message(&loaded.content, loaded.tool_result.as_deref(), cap);
        let expected = classify_unembeddable(&composed).ok_or_else(|| {
            format!(
                "{scenario} {} was marked without a classifier hit",
                target.id
            )
        })?;
        if reason != expected {
            return Err(format!(
                "{scenario} {} marked {reason} but the row classifies as {expected}",
                target.id
            ));
        }
        return Ok(ReplayHit::Marked);
    }
    let stored = store
        .written(target.table, &target.id)
        .await
        .ok_or_else(|| format!("{scenario} {} was not embedded", target.id))?;
    let loaded = load_target(scenario, store, &target).await?;
    check_embedding(scenario, &target.id, &provider, &stored, &loaded).await?;
    Ok(ReplayHit::Embedded)
}

async fn load_target(
    scenario: &str,
    store: &Store,
    target: &EmbedTarget,
) -> Result<LoadedTarget, String> {
    EmbeddingStore::load(store, target)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("{scenario} {} is missing", target.id))
}

async fn check_embedding(
    scenario: &str,
    id: &str,
    provider: &FakeProvider,
    stored: &StoredEmbedding,
    loaded: &LoadedTarget,
) -> Result<(), String> {
    let cap = usize::try_from(settings().tool_result_cap).expect("cap");
    let composed = compose_message(&loaded.content, loaded.tool_result.as_deref(), cap);
    let requests = provider.embed_requests().await;
    let batch = requests
        .first()
        .ok_or_else(|| format!("{scenario} {id} made no embed request"))?;
    if requests.len() != 1 || batch.join("") != composed {
        return Err(format!("{scenario} {id} embed text does not tile the row"));
    }
    if stored.vector.len() != width() || stored.content_hash != content_hash(&composed) {
        return Err(format!("{scenario} {id} stored the wrong embedding"));
    }
    let chunk_text: String = stored
        .chunks
        .iter()
        .map(|chunk| chunk.content.as_str())
        .collect();
    if composed.chars().count() > 6_000 {
        if stored.chunks.len() < 2 || chunk_text != composed {
            return Err(format!("{scenario} {id} did not store composed chunks"));
        }
    } else if !stored.chunks.is_empty() {
        return Err(format!("{scenario} {id} stored chunks for a short row"));
    }
    Ok(())
}

fn step_rows(step: &StepRecord) -> Result<&Json, String> {
    step.rows
        .as_ref()
        .ok_or_else(|| format!("{} has no rows", step.label))
}

fn has_key(rows: &Json, key: &str) -> bool {
    rows.as_object()
        .is_some_and(|object| object.contains_key(key))
}

fn array<'a>(rows: &'a Json, key: &str) -> Result<&'a [Json], String> {
    let object = rows.as_object().ok_or("rows are not an object")?;
    let value = object.get(key).ok_or_else(|| format!("{key} is absent"))?;
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{key} is not an array"))
}

#[derive(Debug, Deserialize)]
struct RecordedJob {
    task: String,
    payload: String,
    key: String,
    priority: i16,
    max_attempts: i32,
    attempts: i32,
    queue: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RecordedMessage {
    id: String,
    content: Option<String>,
    tool_result: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TargetFields {
    #[serde(rename = "targetTable")]
    target_table: String,
    #[serde(rename = "targetId")]
    target_id: String,
}
