use std::time::Duration;

use config::Environment;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

const FIRST_REPLAY: Duration = Duration::from_secs(30);
const REPLAY_EVERY: Duration = Duration::from_secs(300);

pub(super) fn replay_captures(
    tracker: &TaskTracker,
    env: &Environment,
    shutdown: &CancellationToken,
) {
    let env = env.clone();
    let shutdown = shutdown.child_token();
    tracker.spawn(async move {
        let mut beat = tokio::time::interval_at(Instant::now() + FIRST_REPLAY, REPLAY_EVERY);
        beat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = beat.tick() => {}
            }
            if let Err(error) = cli::capture_hook::replay_spool(&env, chrono::Utc::now()).await {
                tracing::debug!(%error, "spooled captures were not resent");
            }
        }
    });
}
