use std::time::Duration;

use kernel::model::ModelCallFailure;
use kernel::model::ModelProviderError;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::error::WorkerError;
use crate::runner::POLL_INTERVAL;

pub(crate) const FLOOR: Duration = Duration::from_secs(1);
pub(crate) const CEILING: Duration = POLL_INTERVAL;

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
pub enum OutageCause {
    #[strum(to_string = "unreachable")]
    Unreachable,
    #[strum(to_string = "rejecting its credentials")]
    Credentials,
    #[strum(to_string = "throttling requests")]
    Throttled,
    #[strum(to_string = "failing with server errors")]
    ServerError,
}

#[derive(Debug)]
struct Held {
    holders: usize,
    down: watch::Sender<Option<OutageCause>>,
}

#[derive(Debug)]
pub(crate) struct OutageGate {
    held: parking_lot::Mutex<Held>,
    state: watch::Receiver<Option<OutageCause>>,
}

impl OutageGate {
    pub(crate) fn new() -> Self {
        let (down, state) = watch::channel(None);
        Self {
            held: parking_lot::Mutex::new(Held { holders: 0, down }),
            state,
        }
    }

    pub(crate) fn cause(&self) -> watch::Receiver<Option<OutageCause>> {
        self.state.clone()
    }

    pub(crate) fn is_down(&self) -> bool {
        self.state.borrow().is_some()
    }

    pub(crate) fn enter(&self, cause: OutageCause) {
        let mut held = self.held.lock();
        held.holders += 1;
        let first = held.holders == 1;
        if first {
            let _previous = held.down.send_replace(Some(cause));
        }
        drop(held);
        if first {
            tracing::warn!(
                "model provider is {cause}; the worker stopped claiming until it recovers"
            );
        }
    }

    pub(crate) fn leave(&self, recovered: bool) {
        let mut held = self.held.lock();
        let Some(remaining) = held.holders.checked_sub(1) else {
            return;
        };
        held.holders = remaining;
        if remaining == 0 {
            let _previous = held.down.send_replace(None);
        }
        drop(held);
        if remaining == 0 && recovered {
            tracing::info!("model provider recovered; the worker resumed claiming");
        }
    }

    pub(crate) async fn wait_until_ready(
        &self,
        cancel: &CancellationToken,
    ) -> Result<bool, crate::error::WorkerError> {
        let mut down = self.state.clone();
        tokio::select! {
            biased;
            () = cancel.cancelled() => Ok(false),
            result = down.wait_for(Option::is_none) => {
                result.map(|_| true).map_err(|_| crate::error::WorkerError::Closed)
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct Hold<'a> {
    gate: &'a OutageGate,
    active: bool,
}

impl<'a> Hold<'a> {
    pub(crate) fn new(gate: &'a OutageGate) -> Self {
        Self {
            gate,
            active: false,
        }
    }

    pub(crate) fn enter(&mut self, cause: OutageCause) {
        if self.active {
            return;
        }
        self.gate.enter(cause);
        self.active = true;
    }

    pub(crate) fn release(&mut self, recovered: bool) {
        if self.active {
            self.gate.leave(recovered);
            self.active = false;
        }
    }
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        self.release(false);
    }
}

pub(crate) fn next_delay(current: Duration) -> Duration {
    match current.checked_mul(2) {
        Some(doubled) if doubled <= CEILING => doubled,
        Some(_) | None => CEILING,
    }
}

pub(crate) async fn wait(delay: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

pub(crate) fn pause_for(current: Duration, retry_after: Option<Duration>) -> Duration {
    match retry_after {
        Some(hint) => hint.max(current),
        None => current,
    }
}

pub(crate) fn provider_outage(error: &ModelProviderError) -> Option<WorkerError> {
    if chain_refused(error) {
        return Some(WorkerError::ProviderOutage {
            cause: OutageCause::Unreachable,
            retry_after: None,
        });
    }
    let (cause, retry_after) = match error.failure()? {
        ModelCallFailure::Http {
            status,
            retry_after,
            ..
        } => (http_cause(*status)?, *retry_after),
        ModelCallFailure::Credentials { .. } => (OutageCause::Credentials, None),
        ModelCallFailure::Unreachable { .. } => (OutageCause::Unreachable, None),
        ModelCallFailure::Transport { .. } => return None,
    };
    Some(WorkerError::ProviderOutage { cause, retry_after })
}

fn http_cause(status: u16) -> Option<OutageCause> {
    match status {
        401 | 403 => Some(OutageCause::Credentials),
        429 => Some(OutageCause::Throttled),
        500..=599 => Some(OutageCause::ServerError),
        _ => None,
    }
}

fn chain_refused(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(item) = current {
        if io_refused(item) || text_refused(&item.to_string()) {
            return true;
        }
        current = item.source();
    }
    false
}

fn io_refused(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::ConnectionRefused)
}

fn text_refused(text: &str) -> bool {
    text.to_ascii_lowercase().contains("connection refused")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::CEILING;
    use super::OutageCause;
    use super::OutageGate;
    use super::next_delay;
    use super::pause_for;

    #[test]
    fn the_gate_is_down_with_the_first_cause_until_the_last_holder_leaves() {
        let gate = OutageGate::new();
        let cause = gate.cause();
        gate.leave(true);
        assert!(!gate.is_down());
        gate.enter(OutageCause::Unreachable);
        gate.enter(OutageCause::Throttled);
        assert_eq!(*cause.borrow(), Some(OutageCause::Unreachable));
        gate.leave(false);
        assert!(gate.is_down());
        assert_eq!(*cause.borrow(), Some(OutageCause::Unreachable));
        gate.leave(true);
        assert!(!gate.is_down());
        assert_eq!(*cause.borrow(), None);
        gate.enter(OutageCause::Throttled);
        assert_eq!(*cause.borrow(), Some(OutageCause::Throttled));
    }

    #[test]
    fn the_backoff_stops_growing_at_the_ceiling() {
        assert_eq!(next_delay(CEILING), CEILING);
    }

    #[test]
    fn a_retry_after_hint_lengthens_but_never_shortens_the_pause() {
        let backoff = Duration::from_secs(8);
        assert_eq!(pause_for(backoff, None), backoff);
        assert_eq!(pause_for(backoff, Some(Duration::ZERO)), backoff);
        assert_eq!(
            pause_for(backoff, Some(Duration::from_secs(300))),
            Duration::from_secs(300)
        );
    }
}
