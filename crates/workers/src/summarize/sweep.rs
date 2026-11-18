use super::{SummarizeEnv, llm::cancelled};
use crate::WorkerError;

pub async fn handle_reap(env: &SummarizeEnv<'_>) -> Result<u64, WorkerError> {
    cancelled(env)?;
    let flagged = env.queue.flag_stuck().await.map_err(WorkerError::queue)?;
    if flagged > 0 {
        tracing::warn!("[reap-dead-jobs] flagged {flagged} job(s) that ran out of attempts");
    }
    Ok(flagged)
}
