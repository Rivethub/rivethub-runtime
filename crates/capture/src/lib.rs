pub mod backfill;
pub mod batch;
pub mod bundle;
pub mod event_id;
pub mod harnesses;
pub mod hook;
pub mod listing;
pub mod payload;
pub mod readers;
pub mod receive;
pub mod records;
pub mod redaction;
pub mod rules;
pub mod sanitize;
pub mod seal;
pub mod share;
pub mod share_policy;
pub mod sign;
pub mod spool;
pub mod store_path;
pub mod transcript;
pub mod writer;

fn session_observation(
    batch: &wire::capture::CaptureBatchRequest,
) -> Option<wire::capture::SessionObservation> {
    match batch.session_observation() {
        Ok(observation) => observation,
        Err(error) => {
            tracing::debug!(%error, "ignoring capture observation with an invalid timestamp");
            None
        }
    }
}
