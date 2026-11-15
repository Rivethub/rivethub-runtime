use crate::common;

use chrono::Duration;
use chrono::Utc;
use common::Ids;
use common::Scenario;
use common::ScenarioFile;
use common::TestResult;
use common::expect_json;
use common::expect_logs;
use common::instant;
use common::recording;
use common::table;
use memory_sqlite::ClaimedSqliteJob;
use memory_sqlite::FailOutcome;
use memory_sqlite::JobInsert;
use memory_sqlite::SqliteError;
use memory_sqlite::SqliteStore;
use memory_sqlite::retry_delay_ms;
use serde::Serialize;
use testkit::Json;

const QUEUE: &str = "queue: enqueue/claim/fail/dead/revive/requeue/recover";
#[derive(Serialize)]
struct Pair {
    n: u32,
    f: f64,
}

#[derive(Serialize)]
struct Revived {
    n: u32,
}

#[tokio::test]
async fn queue_replay_matches_the_recording() -> TestResult {
    let file: ScenarioFile = recording("jobs.json")?;
    let scenario = find(&file, QUEUE)?;
    let mut ids = Ids::default();
    replay_queue(scenario, &mut ids).await?;
    expect_logs(&scenario.logs, &Vec::new(), QUEUE, &mut ids)?;
    Ok(())
}

fn find<'a>(file: &'a ScenarioFile, name: &str) -> TestResult<&'a Scenario> {
    file.scenarios
        .iter()
        .find(|scenario| scenario.name == name)
        .ok_or_else(|| format!("missing scenario {name}").into())
}

async fn replay_queue(scenario: &Scenario, ids: &mut Ids) -> TestResult {
    let fresh = common::fresh_store()?;
    let store = &fresh.store;
    let mut last: Option<ClaimedSqliteJob> = None;
    let mut finished: Option<String> = None;
    for step in &scenario.steps {
        let now = instant(&step.at)?;
        let actual = queue_step(store, step.label.as_str(), now, &mut last, &mut finished).await?;
        if step.label == "claim" {
            finished = Some(required_id(last.as_ref())?);
        }
        expect_json(recorded(step), &actual, &step.label, ids)?;
    }
    Ok(())
}

async fn queue_step(
    store: &SqliteStore,
    label: &str,
    now: chrono::DateTime<Utc>,
    last: &mut Option<ClaimedSqliteJob>,
    finished: &mut Option<String>,
) -> TestResult<Json> {
    if let Some(enqueued) = enqueue_step(store, label, now).await? {
        return Ok(enqueued);
    }
    let actual = match label {
        "claim empty task list" => claim_json(store, Vec::new(), now, last).await?,
        "claim other task" => claim_json(store, vec!["other".to_owned()], now, last).await?,
        "claim t"
        | "claim t (no-key job is older: run_at order)"
        | "claim t again (k1 due)"
        | "claim" => claim_json(store, vec!["t".to_owned()], now, last).await?,
        "fail j1 (retry)" => fail_json(store, last.as_ref(), &"boom ".repeat(500), now).await?,
        "fail j1 (dead)" => fail_json(store, last.as_ref(), "second failure", now).await?,
        "revive k1" => bool_json(
            store
                .revive(
                    "k1".to_owned(),
                    serde_json::to_string(&Revived { n: 4 })?,
                    now,
                )
                .await?,
        ),
        "revive missing" => bool_json(
            store
                .revive("missing".to_owned(), "null".to_owned(), now)
                .await?,
        ),
        "recoverRunning" => Json::from(store.recover_running(now).await?),
        "fail to dead with maxAttempts reached?" => {
            let id = required_id(last.as_ref())?;
            *finished = Some(id.clone());
            lower_max(store, &id).await?;
            fail_json(store, last.as_ref(), "second failure", now).await?
        }
        "requeueDead(u) (none)" => Json::from(store.requeue_dead(Some("u".to_owned()), now).await?),
        "requeueDead(t)" => Json::from(store.requeue_dead(Some("t".to_owned()), now).await?),
        "requeueDead() all" => Json::from(store.requeue_dead(None, now).await?),
        "counts" => serde_json::to_value(store.job_counts().await?)?,
        "complete" => {
            store
                .complete_job(finished.clone().ok_or("no job to complete")?)
                .await?;
            Json::Null
        }
        "retryDelayMs 0..10" => {
            let pairs: Vec<(u32, u64)> = (0..=10)
                .map(|attempt| (attempt, retry_delay_ms(attempt)))
                .collect();
            serde_json::to_value(pairs)?
        }
        label if label.starts_with("rows") => Json::Array(table(store, "ros_jobs").await?),
        label => return Err(format!("unhandled queue step {label}").into()),
    };
    Ok(actual)
}

async fn enqueue_step(
    store: &SqliteStore,
    label: &str,
    now: chrono::DateTime<Utc>,
) -> TestResult<Option<Json>> {
    let json = match label {
        "enqueue k1" | "enqueue k1 again (dedup)" | "enqueue k1 while dead" => bool_json(
            store
                .enqueue_job(
                    JobInsert::new("t", serde_json::to_string(&Pair { n: 1, f: 0.5 })?, now)
                        .key("k1")
                        .max_attempts(2),
                )
                .await?,
        ),
        "enqueue no key, null payload" => bool_json(
            store
                .enqueue_job(JobInsert::new(
                    "t",
                    serde_json::to_string(&None::<u8>)?,
                    now,
                ))
                .await?,
        ),
        "enqueue future runAt" => bool_json(
            store
                .enqueue_job(
                    JobInsert::new("t", serde_json::to_string("later")?, now)
                        .key("k3")
                        .run_at(now + Duration::hours(1)),
                )
                .await?,
        ),
        _ => return Ok(None),
    };
    Ok(Some(json))
}

const NULL_RESULT: Json = Json::Null;

fn recorded(step: &common::Step) -> &Json {
    match step.result {
        Some(ref value) => value,
        None => &NULL_RESULT,
    }
}

async fn claim_json(
    store: &SqliteStore,
    tasks: Vec<String>,
    now: chrono::DateTime<Utc>,
    last: &mut Option<ClaimedSqliteJob>,
) -> TestResult<Json> {
    let claimed = store.claim_tasks(tasks, now).await?;
    let json = match &claimed {
        Some(job) => serde_json::to_value(job)?,
        None => Json::Null,
    };
    if claimed.is_some() {
        *last = claimed;
    }
    Ok(json)
}

async fn fail_json(
    store: &SqliteStore,
    job: Option<&ClaimedSqliteJob>,
    error: &str,
    now: chrono::DateTime<Utc>,
) -> TestResult<Json> {
    let outcome = store
        .fail_job(required_id(job)?, error.to_owned(), now)
        .await?;
    Ok(Json::String(match outcome {
        FailOutcome::Retry => "retry".to_owned(),
        FailOutcome::Dead => "dead".to_owned(),
    }))
}

fn required_id(job: Option<&ClaimedSqliteJob>) -> TestResult<String> {
    job.map(|job| job.id.clone())
        .ok_or_else(|| "no claimed job".into())
}

fn bool_json(value: bool) -> Json {
    Json::Bool(value)
}

async fn lower_max(store: &SqliteStore, id: &str) -> TestResult {
    let id = id.to_owned();
    store
        .with_conn(move |conn| {
            conn.execute(
                "UPDATE ros_jobs SET max_attempts = attempts WHERE id = ?1",
                [id],
            )
            .map_err(|source| SqliteError::Query {
                operation: "test max attempts",
                source,
            })?;
            Ok(())
        })
        .await?;
    Ok(())
}
