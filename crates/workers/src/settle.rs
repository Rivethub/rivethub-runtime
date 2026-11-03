use std::time::Duration;

use kernel::queue::ClaimedJob;
use kernel::queue::JobId;
use kernel::queue::JobQueue;
use tokio_util::sync::CancellationToken;

use crate::dispatch::{JobEnv, dispatch};
use crate::error::WorkerError;
use crate::outage::Hold;
use crate::outage::OutageGate;

const ROUTE_BUSY_DELAY: Duration = Duration::from_secs(30);

struct Attempt<'a> {
    delay: Duration,
    hold: Hold<'a>,
}

pub(crate) async fn settle<Q>(
    gate: &OutageGate,
    queue: &Q,
    env: &JobEnv,
    job: ClaimedJob,
    cancel: &CancellationToken,
) -> Result<(), WorkerError>
where
    Q: JobQueue,
{
    let mut attempt = Attempt {
        delay: crate::outage::FLOOR,
        hold: Hold::new(gate),
    };
    loop {
        if cancel.is_cancelled() {
            return release(queue, job.id, attempt).await;
        }
        let work = dispatch(queue, env, &job, cancel);
        tokio::pin!(work);
        let outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => return release(queue, job.id, attempt).await,
            result = &mut work => result,
        };
        match outcome {
            Ok(()) => return finish_ok(queue, job.id, attempt).await,
            Err(WorkerError::ProviderOutage { cause, retry_after }) => {
                if !pause(&mut attempt, cause, retry_after, cancel).await {
                    return release(queue, job.id, attempt).await;
                }
            }
            Err(WorkerError::Cancelled) => return release(queue, job.id, attempt).await,
            Err(WorkerError::RouteBusy) => return postpone(queue, job.id, attempt).await,
            Err(error) => return finish_err(queue, job.id, attempt, &error).await,
        }
    }
}

async fn pause(
    attempt: &mut Attempt<'_>,
    cause: crate::outage::OutageCause,
    retry_after: Option<Duration>,
    cancel: &CancellationToken,
) -> bool {
    attempt.hold.enter(cause);
    let delay = crate::outage::pause_for(attempt.delay, retry_after);
    let waited = crate::outage::wait(delay, cancel).await;
    attempt.delay = crate::outage::next_delay(attempt.delay);
    waited
}

async fn finish_ok<Q: JobQueue>(
    queue: &Q,
    id: JobId,
    mut attempt: Attempt<'_>,
) -> Result<(), WorkerError> {
    attempt.hold.release(true);
    queue.complete(id).await.map_err(WorkerError::queue)
}

async fn finish_err<Q: JobQueue>(
    queue: &Q,
    id: JobId,
    mut attempt: Attempt<'_>,
    error: &WorkerError,
) -> Result<(), WorkerError> {
    attempt.hold.release(true);
    fail_job(queue, id, error).await
}

async fn release<Q: JobQueue>(
    queue: &Q,
    id: JobId,
    mut attempt: Attempt<'_>,
) -> Result<(), WorkerError> {
    attempt.hold.release(false);
    queue.release(id).await.map_err(WorkerError::queue)
}

async fn postpone<Q: JobQueue>(
    queue: &Q,
    id: JobId,
    mut attempt: Attempt<'_>,
) -> Result<(), WorkerError> {
    attempt.hold.release(true);
    queue
        .postpone(id, ROUTE_BUSY_DELAY)
        .await
        .map_err(WorkerError::queue)
}

async fn fail_job<Q: JobQueue>(
    queue: &Q,
    id: kernel::queue::JobId,
    error: &WorkerError,
) -> Result<(), WorkerError> {
    tracing::warn!("job {id} failed: {error}");
    queue
        .fail(id, &error.to_string())
        .await
        .map_err(WorkerError::queue)
}
