use config::{Config, Environment};
use gateway::account::{CrashJob, install_crash_hook};
use workers::ProcessRole;

pub(crate) fn arm(config: &Config, env: &Environment, job: CrashJob) {
    if config.telemetry_disabled() {
        return;
    }
    match gateway::state_dir(env) {
        Ok(state_dir) => install_crash_hook(&state_dir, job),
        Err(error) => {
            tracing::warn!(%error, "crash reports are off: the state directory is unknown");
        }
    }
}

pub(crate) const fn worker_job(role: ProcessRole) -> CrashJob {
    match role {
        ProcessRole::Embedder => CrashJob::Embedder,
        ProcessRole::Summarizer | ProcessRole::All => CrashJob::Summarizer,
        ProcessRole::Categorizer => CrashJob::Categorizer,
        ProcessRole::WikiBuilder => CrashJob::WikiWriter,
        ProcessRole::EntityBuilder => CrashJob::EntityBuilder,
    }
}
