use kernel::queue::{FixedJob, Job, JobKeyMode, JobName, JobQueue, WorkerRole};
use kernel::wiki::builder::ExtractWiki;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::JsonObject;
use workers::WorkerError;
use workers::embedding_sweep::{WikiSweep, handle_enqueue_stale_wiki};

use super::support::{RecordingQueue, TestResult, seed};

async fn summary(
    pool: &PgPool,
    id: u16,
    kind: &str,
    conversation: Option<Uuid>,
) -> Result<Uuid, sqlx::Error> {
    let offset = f64::from(id);
    let id = Uuid::from_u128(u128::from(id));
    sqlx::query(
        "INSERT INTO rh_summaries (id, conversation_id, kind, content, created_at, owner_user_id)
         VALUES ($1, $2, $3, 'a summary', '2026-10-01T00:00:00Z'::timestamptz + make_interval(secs => $4), 'owner')",
    )
    .bind(id)
    .bind(conversation)
    .bind(kind)
    .bind(offset)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn conversation(pool: &PgPool, id: u128, key: &str) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::from_u128(id);
    sqlx::query(
        "INSERT INTO rh_conversations (id, session_key, agent, channel, owner_user_id) VALUES ($1, $2, 'test', 'test', 'owner')",
    )
    .bind(id)
    .bind(key)
    .execute(pool)
    .await?;
    Ok(id)
}

fn extract(summary_id: Uuid, conversation_id: Option<Uuid>) -> Result<Job, serde_json::Error> {
    Ok(Job {
        name: JobName::Fixed(FixedJob::ExtractWiki),
        payload: JsonObject::from_serialize(&ExtractWiki {
            summary_id,
            conversation_id,
        })?,
        key: Some(format!("wiki-ext-{summary_id}")),
        key_mode: JobKeyMode::KeepRunAt,
        priority: 10,
        run_at: None,
        max_attempts: Some(2),
    })
}

async fn dead_job(
    queue: &RecordingQueue,
    pool: &PgPool,
    summary: Uuid,
    idle_hours: i32,
    locked_hours: Option<i32>,
) -> TestResult {
    let id = queue.inner.enqueue(extract(summary, None)?).await?;
    sqlx::query(
        "UPDATE graphile_worker._private_jobs
            SET attempts = max_attempts, priority = 3,
                run_at = now() - make_interval(hours => $2),
                updated_at = now() - make_interval(hours => $2),
                locked_at = now() - make_interval(hours => $3),
                locked_by = CASE WHEN $3 IS NULL THEN NULL ELSE 'gone' END
          WHERE id = $1",
    )
    .bind(i64::from(id))
    .bind(idle_hours)
    .bind(locked_hours)
    .execute(pool)
    .await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn stale_wiki_re_adds_only_never_extracted_unqueued_leaves(pool: PgPool) -> TestResult {
    let store = seed(&pool).await;
    let queue = RecordingQueue::new(&pool).await;
    let cancel = CancellationToken::new();
    let heartbeat = conversation(&pool, 1, "heartbeat:main").await?;
    let chat = conversation(&pool, 2, "chat:main").await?;
    let near = conversation(&pool, 3, "agent:heartbeat:main").await?;
    summary(&pool, 10, "leaf", Some(heartbeat)).await?;
    let orphan = summary(&pool, 11, "leaf", None).await?;
    let normal = summary(&pool, 12, "leaf", Some(chat)).await?;
    let extracted = summary(&pool, 13, "leaf", Some(chat)).await?;
    let queued = summary(&pool, 14, "leaf", Some(chat)).await?;
    summary(&pool, 15, "branch", Some(chat)).await?;
    let similar = summary(&pool, 16, "leaf", Some(near)).await?;
    let flagged = summary(&pool, 17, "leaf", Some(chat)).await?;
    sqlx::query("INSERT INTO rh_wiki_extractions (summary_id, status, pipeline_version, owner_user_id) VALUES ($1, 'failed', 1, 'owner')")
        .bind(extracted)
        .execute(&pool)
        .await?;
    queue.inner.enqueue(extract(queued, Some(chat))?).await?;
    sqlx::query("UPDATE graphile_worker._private_jobs SET attempts = max_attempts")
        .execute(&pool)
        .await?;
    assert_eq!(
        handle_enqueue_stale_wiki(&store, &queue, false, 20, &cancel).await?,
        WikiSweep {
            revived: 0,
            added: 0
        }
    );
    assert_eq!(
        handle_enqueue_stale_wiki(&store, &queue, true, 0, &cancel).await?,
        WikiSweep {
            revived: 0,
            added: 0
        }
    );
    assert!(queue.jobs.lock().is_empty());
    sqlx::query("UPDATE rh_summaries SET created_at = now() WHERE id = $1")
        .bind(normal)
        .execute(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO rh_flagged_jobs (task, job_key, payload, priority, max_attempts, failures,
             last_error, first_failed_at, last_failed_at, flagged_at)
         VALUES ('extract-wiki', $1, '{}', 10, 2, 2, 'boom', now(), now(), now())",
    )
    .bind(format!("wiki-ext-{flagged}"))
    .execute(&pool)
    .await?;
    assert_eq!(
        handle_enqueue_stale_wiki(&store, &queue, true, 20, &cancel).await?,
        WikiSweep {
            revived: 0,
            added: 3
        }
    );
    let mut recent = extract(normal, Some(chat))?;
    recent.priority = -10;
    assert_eq!(
        *queue.jobs.lock(),
        vec![
            recent,
            extract(similar, Some(near))?,
            extract(orphan, None)?
        ]
    );
    let stored: (bool, String) = sqlx::query_as(
        "SELECT payload::jsonb ? 'conversationId', payload::jsonb ->> 'summaryId'
           FROM graphile_worker._private_jobs WHERE key = $1",
    )
    .bind(format!("wiki-ext-{orphan}"))
    .fetch_one(&pool)
    .await?;
    assert_eq!(stored, (false, orphan.to_string()));
    queue.jobs.lock().clear();
    assert_eq!(
        handle_enqueue_stale_wiki(&store, &queue, true, 20, &cancel).await?,
        WikiSweep {
            revived: 0,
            added: 0
        }
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn stale_wiki_revives_dead_jobs_first_and_splits_the_cap(pool: PgPool) -> TestResult {
    let store = seed(&pool).await;
    let queue = RecordingQueue::new(&pool).await;
    let cancel = CancellationToken::new();
    summary(&pool, 20, "leaf", None).await?;
    let newest = summary(&pool, 21, "leaf", None).await?;
    dead_job(&queue, &pool, Uuid::from_u128(30), 30, None).await?;
    dead_job(&queue, &pool, Uuid::from_u128(31), 29, Some(5)).await?;
    dead_job(&queue, &pool, Uuid::from_u128(32), 28, Some(1)).await?;
    dead_job(&queue, &pool, Uuid::from_u128(33), 2, None).await?;
    queue.jobs.lock().clear();
    assert_eq!(
        handle_enqueue_stale_wiki(&store, &queue, true, 3, &cancel).await?,
        WikiSweep {
            revived: 2,
            added: 1
        }
    );
    assert_eq!(*queue.jobs.lock(), vec![extract(newest, None)?]);
    let rows: Vec<(String, i16, i16, bool, bool)> = sqlx::query_as(
        "SELECT key, attempts, priority, locked_at IS NULL AND locked_by IS NULL, run_at > now() - interval '1 minute'
           FROM graphile_worker._private_jobs WHERE key LIKE 'wiki-ext-%' ORDER BY key",
    )
    .fetch_all(&pool)
    .await?;
    let key = |id: u128| format!("wiki-ext-{}", Uuid::from_u128(id));
    assert_eq!(
        rows,
        vec![
            (key(21), 0, 10, true, true),
            (key(30), 0, 10, true, true),
            (key(31), 0, 10, true, true),
            (key(32), 2, 3, false, false),
            (key(33), 2, 3, true, false),
        ]
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn revived_counts_what_reschedule_jobs_returned(pool: PgPool) -> TestResult {
    let store = seed(&pool).await;
    let queue = RecordingQueue::new(&pool).await;
    let cancel = CancellationToken::new();
    for id in 40..43 {
        dead_job(&queue, &pool, Uuid::from_u128(id), 30, None).await?;
    }
    sqlx::raw_sql(
        "CREATE OR REPLACE FUNCTION graphile_worker.reschedule_jobs(job_ids bigint[], run_at timestamptz DEFAULT NULL,
             priority integer DEFAULT NULL, attempts integer DEFAULT NULL, max_attempts integer DEFAULT NULL)
           RETURNS SETOF graphile_worker._private_jobs LANGUAGE sql AS $$
             UPDATE graphile_worker._private_jobs AS jobs
                SET run_at = reschedule_jobs.run_at, priority = reschedule_jobs.priority::smallint,
                    attempts = reschedule_jobs.attempts::smallint, updated_at = now()
              WHERE jobs.id = job_ids[1]
             RETURNING *
           $$",
    )
    .execute(&pool)
    .await?;
    assert_eq!(
        handle_enqueue_stale_wiki(&store, &queue, true, 1, &cancel).await?,
        WikiSweep {
            revived: 1,
            added: 0
        }
    );
    assert_eq!(
        handle_enqueue_stale_wiki(&store, &queue, true, 5, &cancel).await?,
        WikiSweep {
            revived: 1,
            added: 0
        }
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_failed_add_fails_the_stale_wiki_job(pool: PgPool) -> TestResult {
    let store = seed(&pool).await;
    let queue = RecordingQueue::new(&pool).await;
    let cancel = CancellationToken::new();
    summary(&pool, 50, "leaf", None).await?;
    sqlx::query("ALTER FUNCTION graphile_worker.add_job(text,json,text,timestamptz,int,text,int,text[],text) RENAME TO add_job_parked")
        .execute(&pool)
        .await?;
    let failed = handle_enqueue_stale_wiki(&store, &queue, true, 20, &cancel).await;
    assert!(matches!(failed, Err(WorkerError::Queue(_))), "{failed:?}");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn cron_registration_and_due_jobs_follow_roles_and_wiki_switch(pool: PgPool) -> TestResult {
    let queue = RecordingQueue::new(&pool).await;
    let cancel = CancellationToken::new();
    let entries: Vec<_> = workers::ProcessRole::All
        .selected()
        .into_iter()
        .flat_map(|role| role.crontab(true))
        .collect();
    assert_eq!(entries.len(), 12);
    assert_eq!(WorkerRole::WikiBuilder.crontab(false).len(), 1);
    assert!(entries.iter().all(|entry| entry.backfill_seconds == 0));
    workers::schedule::enqueue_due(&queue, &entries, "2026-10-16T12:30:00Z".parse()?, &cancel)
        .await?;
    let expected: Vec<_> = [
        (FixedJob::EnqueueUnembedded, "unembedded-sweep"),
        (FixedJob::EnqueueUnchunked, "unchunked-sweep"),
        (FixedJob::EnqueueIdle, "idle-enqueue"),
        (FixedJob::EnqueueStaleSummaries, "stale-compaction-sweep"),
        (FixedJob::PurgeDeletedSessions, "deleted-session-sweep"),
        (FixedJob::EnqueueWikiBackfill, "wiki-backfill"),
        (FixedJob::EnqueueStaleWiki, "stale-wiki-sweep"),
        (FixedJob::EnqueueEntityBackfill, "entity-backfill"),
        (FixedJob::EnqueueStaleEntities, "stale-entity-sweep"),
    ]
    .into_iter()
    .map(|(task, key)| Job {
        name: JobName::Fixed(task),
        payload: JsonObject::default(),
        key: Some(key.to_owned()),
        key_mode: JobKeyMode::KeepRunAt,
        priority: kernel::queue::MAINTENANCE_PRIORITY,
        run_at: None,
        max_attempts: Some(25),
    })
    .collect();
    assert_eq!(*queue.jobs.lock(), expected);
    Ok(())
}
