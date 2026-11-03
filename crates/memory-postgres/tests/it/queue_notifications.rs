use std::time::Duration;

use kernel::queue::{FixedJob, Job, JobKeyMode, JobName, JobQueue, WorkerRole};
use memory_postgres::PostgresQueue;
use sqlx::PgPool;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn subscribed_workers_observe_an_insert_from_another_connection(pool: PgPool) {
    let queue = PostgresQueue::connect(pool).await.expect("queue");
    let mut notifications = queue.subscribe().await.expect("LISTEN before claim");
    assert!(
        queue
            .claim(&kernel::queue::ClaimSelector::matching(
                queue.task_names(WorkerRole::Embedder)
            ))
            .await
            .expect("claim")
            .is_none()
    );
    queue
        .enqueue(Job {
            name: JobName::Fixed(FixedJob::EmbedTarget),
            payload: wire::JsonObject::default(),
            key: None,
            key_mode: JobKeyMode::Replace,
            priority: 0,
            run_at: None,
            max_attempts: None,
        })
        .await
        .expect("enqueue");
    tokio::time::timeout(Duration::from_secs(5), notifications.recv())
        .await
        .expect("notification arrives")
        .expect("notification");
    let claimed = queue
        .claim(&kernel::queue::ClaimSelector::matching(
            queue.task_names(WorkerRole::Embedder),
        ))
        .await
        .expect("claim")
        .expect("job");
    queue.complete(claimed.id).await.expect("complete");
}
