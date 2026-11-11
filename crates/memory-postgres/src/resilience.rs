use std::time::Duration;

use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use sqlx::PgPool;

use crate::PostgresMemory;

async fn terminate_other_backends(pool: &PgPool) {
    sqlx::query(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
         WHERE datname = current_database() AND pid <> pg_backend_pid() \
           AND backend_type = 'client backend'",
    )
    .execute(pool)
    .await
    .expect("terminate backends");
}

#[sqlx::test(migrations = false)]
async fn job_notifications_resume_after_the_backend_is_terminated(pool: PgPool) {
    sqlx::raw_sql(include_str!(
        "../../rivethub/tests/fixtures/graphile_worker.txt"
    ))
    .execute(&pool)
    .await
    .expect("graphile schema");
    let queue = PostgresMemory::new(pool.clone())
        .queue()
        .await
        .expect("queue");
    let mut notifications = queue.subscribe().await.expect("listen");
    terminate_other_backends(&pool).await;
    tokio::time::timeout(Duration::from_secs(20), notifications.recv())
        .await
        .expect("reconnect kick")
        .expect("kick");
    queue
        .enqueue(Job {
            name: JobName::Fixed(FixedJob::ReapDeadJobs),
            payload: wire::JsonObject::default(),
            key: None,
            key_mode: JobKeyMode::KeepRunAt,
            priority: 0,
            run_at: None,
            max_attempts: Some(1),
        })
        .await
        .expect("enqueue");
    tokio::time::timeout(Duration::from_secs(20), notifications.recv())
        .await
        .expect("notification after reconnect")
        .expect("notification");
}
