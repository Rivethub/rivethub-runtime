use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobId;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue as _;
use kernel::queue::JobQueueError;
use kernel::queue::WorkerRole;
use memory_sqlite::SqliteStore;
use serde::Serialize;
use testkit::Json;
use wire::JsonObject;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Serialize)]
struct Target<'a> {
    #[serde(rename = "targetTable")]
    target_table: &'a str,
    #[serde(rename = "targetId")]
    target_id: &'a str,
}

fn payload(target: &str) -> TestResult<JsonObject> {
    Ok(JsonObject::from_serialize(&Target {
        target_table: "ros_messages",
        target_id: target,
    })?)
}

fn job(target: &str, mode: JobKeyMode) -> TestResult<Job> {
    keyed(target, "embed-ros_messages-abc", mode)
}

fn keyed(target: &str, key: &str, mode: JobKeyMode) -> TestResult<Job> {
    Ok(Job {
        name: JobName::Fixed(FixedJob::EmbedTarget),
        payload: payload(target)?,
        key: Some(key.to_owned()),
        key_mode: mode,
        priority: 7,
        run_at: None,
        max_attempts: None,
    })
}

struct Opened {
    _dir: tempfile::TempDir,
    store: SqliteStore,
}

fn open() -> TestResult<Opened> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite_at(&path, chrono::Utc::now())?;
    let store = SqliteStore::open(&path)?;
    Ok(Opened { _dir: dir, store })
}

async fn jobs(store: &SqliteStore) -> TestResult<Vec<Json>> {
    let rows = store
        .with_conn(|conn| {
            Ok(testkit::query_json(
                conn,
                "SELECT * FROM ros_jobs ORDER BY rowid",
                &[],
            ))
        })
        .await??;
    Ok(rows)
}

fn cell<'a>(row: &'a Json, key: &str) -> &'a Json {
    row.get(key).unwrap_or(&Json::Null)
}

#[tokio::test]
async fn the_store_implements_the_job_queue_port() -> TestResult {
    let opened = open()?;
    let store = &opened.store;
    store.enqueue(job("abc", JobKeyMode::Replace)?).await?;
    let queued = jobs(store).await?;
    let row = queued.first().ok_or("enqueued job")?;
    assert_eq!(cell(row, "task"), &Json::from("embed-target"));
    assert_eq!(cell(row, "job_key"), &Json::from("embed-ros_messages-abc"));
    assert_eq!(cell(row, "max_attempts"), &Json::from(5_i64));
    assert_eq!(cell(row, "attempts"), &Json::from(0_i64));
    assert!(
        store
            .claim(&kernel::queue::ClaimSelector::matching(
                store.task_names(WorkerRole::Summarizer)
            ))
            .await?
            .is_none(),
        "a summarizer does not take embed jobs"
    );

    let claimed = store
        .claim(&kernel::queue::ClaimSelector::matching(
            store.task_names(WorkerRole::Embedder),
        ))
        .await?
        .ok_or("embedder claims the job")?;
    assert_eq!(claimed.name, JobName::Fixed(FixedJob::EmbedTarget));
    assert_eq!(claimed.attempt, 1);
    assert_eq!(
        claimed.payload.get("targetId").and_then(Json::as_str),
        Some("abc")
    );
    assert!(
        store
            .claim(&kernel::queue::ClaimSelector::matching(
                store.task_names(WorkerRole::Embedder)
            ))
            .await?
            .is_none()
    );

    store.fail(claimed.id, "boom").await?;
    assert!(
        store
            .claim(&kernel::queue::ClaimSelector::matching(
                store.task_names(WorkerRole::Embedder)
            ))
            .await?
            .is_none(),
        "a failed job waits for its retry"
    );
    store.complete(claimed.id).await?;
    assert_eq!(jobs(store).await?, Vec::<Json>::new());

    let missing = store.complete(JobId::new(9_999)).await;
    assert!(matches!(missing, Err(JobQueueError::Missing { .. })));

    store.enqueue(job("keep", JobKeyMode::KeepRunAt)?).await?;
    store.enqueue(job("other", JobKeyMode::KeepRunAt)?).await?;
    let kept = jobs(store).await?;
    let kept_payload = cell(kept.first().ok_or("kept job")?, "payload")
        .as_str()
        .ok_or("payload text")?;
    assert!(kept_payload.contains("keep"), "{kept_payload}");
    assert!(!kept_payload.contains("other"), "{kept_payload}");

    store.enqueue(job("next", JobKeyMode::Replace)?).await?;
    let replaced = jobs(store).await?;
    let replaced_payload = cell(replaced.first().ok_or("replaced job")?, "payload")
        .as_str()
        .ok_or("payload text")?;
    assert!(replaced_payload.contains("next"), "{replaced_payload}");
    Ok(())
}

#[tokio::test]
async fn arming_keeps_a_waiting_job_in_line_and_only_raises_it() -> TestResult {
    let opened = open()?;
    let store = &opened.store;
    let arm = |target: &str, priority: i16| -> TestResult<Job> {
        Ok(Job {
            priority,
            run_at: Some(chrono::Utc::now()),
            ..keyed(target, "compact-c1", JobKeyMode::Arm)?
        })
    };
    store.enqueue(arm("first", 10)?).await?;
    let queued_at = cell(jobs(store).await?.first().ok_or("armed job")?, "run_at").clone();
    store.enqueue(arm("later", 10)?).await?;
    store.enqueue(arm("urgent", 0)?).await?;
    store.enqueue(arm("demoted", 10)?).await?;
    let rows = jobs(store).await?;
    let [row] = rows.as_slice() else {
        return Err("one armed job".into());
    };
    assert_eq!(cell(row, "run_at"), &queued_at);
    assert!(
        cell(row, "payload")
            .as_str()
            .is_some_and(|text| text.contains("urgent"))
    );
    let priority = store
        .with_conn(|conn| {
            Ok(testkit::query_json(
                conn,
                "SELECT COALESCE((SELECT priority FROM rh_job_priorities), 0) AS priority",
                &[],
            ))
        })
        .await??;
    assert_eq!(
        priority.first().map(|row| cell(row, "priority").clone()),
        Some(Json::from(0_i64))
    );
    Ok(())
}

#[tokio::test]
async fn complete_of_a_finished_claim_does_not_take_the_next_job() -> TestResult {
    let opened = open()?;
    let store = &opened.store;
    store
        .enqueue(keyed(
            "first",
            "embed-ros_messages-first",
            JobKeyMode::Replace,
        )?)
        .await?;
    let claimed = store
        .claim(&kernel::queue::ClaimSelector::matching(
            store.task_names(WorkerRole::Embedder),
        ))
        .await?
        .ok_or("claim")?;
    store.complete(claimed.id).await?;
    store
        .enqueue(keyed(
            "second",
            "embed-ros_messages-second",
            JobKeyMode::Replace,
        )?)
        .await?;
    let again = store.complete(claimed.id).await;
    assert!(matches!(again, Err(JobQueueError::Missing { .. })));
    let rows = jobs(store).await?;
    let row = rows.first().ok_or("the second job")?;
    if rows.len() != 1 {
        return Err(format!("complete removed {rows:?}").into());
    }
    assert_eq!(
        cell(row, "job_key"),
        &Json::from("embed-ros_messages-second")
    );
    Ok(())
}

#[tokio::test]
async fn a_null_payload_is_dead_on_claim() -> TestResult {
    let opened = open()?;
    let store = &opened.store;
    store
        .enqueue_job(
            memory_sqlite::JobInsert::new("embed-target", "null".to_owned(), chrono::Utc::now())
                .key("poison"),
        )
        .await?;
    store
        .enqueue(keyed(
            "alive",
            "embed-ros_messages-alive",
            JobKeyMode::Replace,
        )?)
        .await?;
    let rejected = store
        .claim(&kernel::queue::ClaimSelector::matching(
            store.task_names(WorkerRole::Embedder),
        ))
        .await;
    assert!(matches!(rejected, Err(JobQueueError::Claim { .. })));
    let claimed = store
        .claim(&kernel::queue::ClaimSelector::matching(
            store.task_names(WorkerRole::Embedder),
        ))
        .await?
        .ok_or("the object payload is still claimable")?;
    assert_eq!(
        claimed.payload.get("targetId").and_then(Json::as_str),
        Some("alive")
    );
    let rows = jobs(store).await?;
    let poison = row_with(&rows, "job_key", "poison")?;
    assert_eq!(cell(poison, "state"), &Json::from("dead"));
    let alive = row_with(&rows, "job_key", "embed-ros_messages-alive")?;
    assert_eq!(cell(alive, "state"), &Json::from("running"));
    Ok(())
}

fn row_with<'a>(rows: &'a [Json], key: &str, value: &str) -> TestResult<&'a Json> {
    rows.iter()
        .find(|row| cell(row, key) == &Json::from(value))
        .ok_or_else(|| format!("missing {key} {value}").into())
}

#[tokio::test]
async fn a_job_with_no_key_stores_null() -> TestResult {
    let opened = open()?;
    let store = &opened.store;
    let first = Job {
        name: JobName::Fixed(FixedJob::EmbedTarget),
        payload: payload("one")?,
        key: None,
        key_mode: JobKeyMode::Replace,
        priority: 0,
        run_at: None,
        max_attempts: None,
    };
    store.enqueue(first.clone()).await?;
    let second = Job {
        payload: payload("two")?,
        ..first
    };
    store.enqueue(second).await?;
    let rows = jobs(store).await?;
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert!(cell(row, "job_key").is_null());
    }
    Ok(())
}

#[tokio::test]
async fn queue_notifications_survive_the_empty_claim_to_wait_transition() -> TestResult {
    let opened = open()?;
    let store = &opened.store;
    let mut notifications = store.subscribe().await?;
    assert!(
        store
            .claim(&kernel::queue::ClaimSelector::matching(
                store.task_names(WorkerRole::Embedder)
            ))
            .await?
            .is_none()
    );
    store.enqueue(job("notified", JobKeyMode::Replace)?).await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), notifications.recv()).await??;
    let claimed = store
        .claim(&kernel::queue::ClaimSelector::matching(
            store.task_names(WorkerRole::Embedder),
        ))
        .await?
        .ok_or("claim after notification")?;
    store.complete(claimed.id).await?;
    Ok(())
}

#[tokio::test]
async fn a_pending_sweep_is_queued_once() -> TestResult {
    let opened = open()?;
    testkit::sweeps::a_pending_sweep_is_queued_once(&opened.store).await
}

#[tokio::test]
async fn the_sweeps_run_before_an_older_backlog() -> TestResult {
    let opened = open()?;
    testkit::sweeps::the_sweeps_run_before_an_older_backlog(&opened.store).await
}

#[tokio::test]
async fn a_dead_job_is_named_by_the_conversation_it_worked_on() -> TestResult {
    use kernel::flagged::FlaggedJobs as _;
    let opened = open()?;
    let store = &opened.store;
    store
        .with_conn(|conn| {
            Ok(conn.execute_batch(
                "INSERT INTO ros_conversations (id, session_key, agent, title, created_at, updated_at)
                 VALUES ('c1', 'claude-code:one', 'claude', 'Fix the flaky gate', '2026-10-30T10:00:00Z',
                         '2026-10-30T10:00:00Z');
                 INSERT INTO ros_jobs (id, task, job_key, payload, run_at, attempts, state, created_at, updated_at)
                 VALUES ('j1', 'compact-conversation', 'named', '{\"conversationId\":\"c1\"}',
                         '2026-10-30T10:00:00Z', 5, 'dead', '2026-10-30T10:00:00Z', '2026-10-30T10:02:00Z'),
                        ('j2', 'embed-target', 'bare', 'null', '2026-10-30T10:00:00Z', 5, 'dead',
                         '2026-10-30T10:00:00Z', '2026-10-30T10:01:00Z');",
            ))
        })
        .await??;
    let named: Vec<(Option<String>, Option<String>)> = store
        .flagged()
        .await?
        .into_iter()
        .map(|job| (job.key, job.subject))
        .collect();
    assert_eq!(
        named,
        [
            (
                Some("named".to_owned()),
                Some("Fix the flaky gate".to_owned())
            ),
            (Some("bare".to_owned()), None)
        ]
    );
    Ok(())
}
