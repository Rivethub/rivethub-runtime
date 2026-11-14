use crate::process::guard::SpawnGuarded;
use std::process::Stdio;
use std::time::Duration;

use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue as _;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use wire::JsonObject;

use super::{
    TestResult, install_queue, interrupt, provider_at, provider_config, terminate, write_state,
};

#[derive(Debug, sqlx::FromRow)]
struct JobTally {
    n: i64,
    max_attempts: Option<i64>,
    attempts: Option<i64>,
}

fn reap_job() -> Job {
    Job {
        name: JobName::Fixed(FixedJob::ReapDeadJobs),
        payload: JsonObject::default(),
        key: None,
        key_mode: JobKeyMode::KeepRunAt,
        priority: 0,
        run_at: None,
        max_attempts: None,
    }
}

async fn tally(pool: &PgPool) -> Result<JobTally, sqlx::Error> {
    sqlx::query_as(
        "SELECT count(*)::bigint AS n, \
                max(j.max_attempts)::bigint AS max_attempts, \
                max(j.attempts)::bigint AS attempts \
           FROM graphile_worker._private_jobs j \
           JOIN graphile_worker._private_tasks t ON t.id = j.task_id \
          WHERE t.identifier = $1",
    )
    .bind("reap-dead-jobs")
    .fetch_one(pool)
    .await
}

async fn wait_until_reaped(pool: &PgPool, child: &mut tokio::process::Child) -> Result<(), String> {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return Err(format!("worker exited before the job completed: {status}"));
                }
                Ok(None) => {}
                Err(error) => return Err(error.to_string()),
            }
            match tally(pool).await {
                Ok(row) if row.n == 0 => return Ok(()),
                Ok(_) => tokio::task::yield_now().await,
                Err(error) => return Err(error.to_string()),
            }
        }
    })
    .await;
    match result {
        Ok(inner) => inner,
        Err(_) => match tally(pool).await {
            Ok(row) => Err(format!(
                "reap-dead-jobs was not completed: {} row(s), max_attempts={:?}, attempts={:?}",
                row.n, row.max_attempts, row.attempts
            )),
            Err(error) => Err(error.to_string()),
        },
    }
}

#[tokio::test]
async fn worker_claims_a_reap_job_and_stops_on_sigterm() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
    migrate::postgres_pool(&pool).await?;
    install_queue(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let queue = memory.queue().await?;

    queue.enqueue(reap_job()).await?;
    let url = db.served_url().await?;
    let home = tempfile::tempdir()?;
    write_state(home.path(), &provider_config(home.path(), &url, ""))?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_rh"))
        .args(["worker", "--role", "summarizer"])
        .env_clear()
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn_guarded()?;
    let stderr = child.stderr.take().ok_or("missing stderr")?;
    let logs = tokio::spawn(async move {
        let mut stderr = stderr;
        let mut text = String::new();
        stderr.read_to_string(&mut text).await?;
        Ok::<_, std::io::Error>(text)
    });
    if let Err(error) = wait_until_reaped(&pool, &mut child).await {
        let pid = child.id();
        if let Some(pid) = pid {
            terminate(pid)?;
        }
        let text = logs.await??;
        return Err(format!("{error}\n{text}").into());
    }
    let pid = child.id().ok_or("worker already stopped")?;
    terminate(pid)?;
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait()).await??;
    let text = tokio::time::timeout(Duration::from_secs(30), logs).await???;
    assert!(status.success(), "worker exit: {status}\n{text}");
    assert!(text.contains("SIGTERM received — shutting down"), "{text}");
    assert!(text.contains("worker stopped"), "{text}");
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct HeldJob {
    attempts: i64,
    locked: bool,
    locked_by: Option<String>,
    last_error: Option<String>,
    run_at: String,
}

async fn embed_row(pool: &PgPool) -> Result<HeldJob, sqlx::Error> {
    sqlx::query_as(
        "SELECT j.attempts::bigint AS attempts, \
                (j.locked_at IS NOT NULL) AS locked, \
                j.locked_by, j.last_error, j.run_at::text AS run_at \
           FROM graphile_worker._private_jobs j \
           JOIN graphile_worker._private_tasks t ON t.id = j.task_id \
          WHERE t.identifier = 'embed-target'",
    )
    .fetch_one(pool)
    .await
}

async fn wait_until_held(
    pool: &PgPool,
    child: &mut tokio::process::Child,
) -> Result<HeldJob, String> {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return Err(format!(
                        "worker exited before the job was claimed: {status}"
                    ));
                }
                Ok(None) => {}
                Err(error) => return Err(error.to_string()),
            }
            match embed_row(pool).await {
                Ok(row) if row.locked && row.attempts == 1 => return Ok(row),
                Ok(_) => tokio::task::yield_now().await,
                Err(error) => return Err(error.to_string()),
            }
        }
    })
    .await;
    match result {
        Ok(inner) => inner,
        Err(_) => Err("embed-target was not claimed".to_owned()),
    }
}

#[derive(serde::Serialize)]
struct EmbedTargetPayload<'a> {
    #[serde(rename = "targetTable")]
    target_table: &'a str,
    #[serde(rename = "targetId")]
    target_id: &'a str,
}

async fn seed_embed(pool: &PgPool) -> TestResult<HeldJob> {
    let mut tx = pool.begin().await?;
    sqlx::query("select set_config('rivet.defer_embed_enqueue', 'on', true)")
        .execute(&mut *tx)
        .await?;
    let conversation: String = sqlx::query_scalar(
        "insert into rh_conversations (session_key, agent, owner_user_id) values ('embed-shutdown', 'rivet', 'owner') returning id::text",
    )
    .fetch_one(&mut *tx)
    .await?;
    let message: String = sqlx::query_scalar(
        "insert into rh_messages (conversation_id, agent, channel, role, content, owner_user_id)
         values ($1::uuid, 'rivet', 'test', 'user', 'hello ledger', 'owner')
         returning id::text",
    )
    .bind(&conversation)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let memory = PostgresMemory::new(pool.clone());
    let queue = memory.queue().await?;
    queue
        .enqueue(Job {
            name: JobName::Fixed(FixedJob::EmbedTarget),
            payload: wire::JsonObject::from_serialize(&EmbedTargetPayload {
                target_table: "ros_messages",
                target_id: &message,
            })?,
            key: Some(format!("embed-ros_messages-{message}")),
            key_mode: JobKeyMode::Replace,
            priority: 0,
            run_at: None,
            max_attempts: None,
        })
        .await?;
    let before = embed_row(pool).await?;
    assert!(!before.locked, "{before:?}");
    assert_eq!(before.attempts, 0);
    Ok(before)
}

#[tokio::test]
async fn worker_blocked_on_a_provider_releases_the_job_on_sigint() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
    migrate::postgres_pool(&pool).await?;
    install_queue(&pool).await?;
    let sink = super::blackhole().await?;
    let before = seed_embed(&pool).await?;
    let url = db.served_url().await?;
    let home = tempfile::tempdir()?;
    write_state(
        home.path(),
        &provider_at(home.path(), &url, &format!("http://{}", sink.address), ""),
    )?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_rh"))
        .args(["worker", "--role", "embedder"])
        .env_clear()
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn_guarded()?;
    let stderr = child.stderr.take().ok_or("missing stderr")?;
    let logs = tokio::spawn(async move {
        let mut stderr = stderr;
        let mut text = String::new();
        stderr.read_to_string(&mut text).await?;
        Ok::<_, std::io::Error>(text)
    });
    if let Err(error) = wait_until_held(&pool, &mut child).await {
        if let Some(pid) = child.id() {
            interrupt(pid)?;
        }
        let text = logs.await??;
        return Err(format!("{error}\n{text}").into());
    }
    let pid = child.id().ok_or("worker already stopped")?;
    let started = tokio::time::Instant::now();
    interrupt(pid)?;
    let waited = tokio::time::timeout(kernel::shutdown::SHUTDOWN_GRACE, child.wait()).await;
    let elapsed = started.elapsed();
    let Ok(status) = waited else {
        child.start_kill()?;
        let text = logs.await??;
        return Err(format!("worker still running after {elapsed:?}\n{text}").into());
    };
    let status = status?;
    let text = tokio::time::timeout(Duration::from_secs(5), logs).await???;
    assert!(
        elapsed < kernel::shutdown::SHUTDOWN_GRACE,
        "elapsed {elapsed:?}\n{text}"
    );
    assert!(status.success(), "worker exit: {status}\n{text}");
    assert!(text.contains("SIGINT received — shutting down"), "{text}");
    assert!(text.contains("worker stopped"), "{text}");
    assert!(!text.contains("the worker was cancelled"), "{text}");
    let after = embed_row(&pool).await?;
    assert_eq!(after.attempts, 0, "{after:?}");
    assert!(!after.locked, "{after:?}");
    assert_eq!(after.locked_by, None);
    assert_eq!(after.last_error, None);
    assert_eq!(after.run_at, before.run_at);
    Ok(())
}
