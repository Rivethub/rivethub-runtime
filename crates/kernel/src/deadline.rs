use std::future::Future;
use std::time::Duration;
use std::time::Instant;

tokio::task_local! {
    static DEADLINE: Option<Instant>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadline(Option<Instant>);

impl Deadline {
    #[must_use]
    pub fn after(limit: Duration) -> Self {
        Self(Instant::now().checked_add(limit))
    }

    #[must_use]
    pub const fn unbounded() -> Self {
        Self(None)
    }

    pub async fn run<F: Future>(self, work: F) -> F::Output {
        DEADLINE.scope(self.0, work).await
    }

    #[must_use]
    pub fn remaining() -> Option<Duration> {
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "PATTERNS FALLBACK: a real absence, as work outside a bounded request has no deadline and its statements keep the server's own limit"
            )
        )]
        let at = DEADLINE.try_with(|at| *at).ok().flatten();
        at.map(|at| at.saturating_duration_since(Instant::now()))
    }
}
