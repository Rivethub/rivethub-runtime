use kernel::memory::summary_opening::{OpeningConsumer, OpeningJob};
use kernel::queue::{Job, JobKeyMode};
use uuid::Uuid;
use wire::JsonObject;

use crate::error::WorkerError;

pub fn file_conversation_job(conversation_id: Uuid) -> Result<Job, WorkerError> {
    let mut job = OpeningConsumer::Filing
        .job(conversation_id)
        .map_err(WorkerError::file_payload)?;
    job.key_mode = JobKeyMode::Replace;
    Ok(job)
}

pub(crate) fn read_payload(payload: &JsonObject) -> Result<OpeningJob, WorkerError> {
    payload.deserialize().map_err(WorkerError::file_payload)
}
