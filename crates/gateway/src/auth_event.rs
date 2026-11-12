use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;

const REFUSAL_LOG_CAP: usize = 100;
const WINDOW: Duration = Duration::from_secs(60);
const FLUSH_TICK: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AuthEvent {
    LoginCodeMinted,
    LoginCodeRefused,
    LoginCodeRateLimited,
    ExchangeSucceeded,
    ExchangeRefused,
    ExchangeRateLimited,
    Logout,
    Rotated,
    BearerRejected,
    ForwardedTokenRefused,
}

impl AuthEvent {
    const fn name(self) -> &'static str {
        match self {
            Self::LoginCodeMinted => "login_code_minted",
            Self::LoginCodeRefused => "login_code_refused",
            Self::LoginCodeRateLimited => "login_code_rate_limited",
            Self::ExchangeSucceeded => "exchange_succeeded",
            Self::ExchangeRefused => "exchange_refused",
            Self::ExchangeRateLimited => "exchange_rate_limited",
            Self::Logout => "logout",
            Self::Rotated => "rotated",
            Self::BearerRejected => "bearer_rejected",
            Self::ForwardedTokenRefused => "forwarded_token_refused",
        }
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::LoginCodeMinted => "the owner token minted a one-time login code",
            Self::LoginCodeRefused => "a login code was asked for without the owner token",
            Self::LoginCodeRateLimited => "login codes are rate limited for this peer",
            Self::ExchangeSucceeded => "a login code was exchanged for a browser session",
            Self::ExchangeRefused => "the login code was unknown, expired or already used",
            Self::ExchangeRateLimited => "too many failed login attempts from this peer",
            Self::Logout => "the browser session ended",
            Self::Rotated => "the local owner token was replaced and every session ended",
            Self::BearerRejected => "the bearer token matched no credential",
            Self::ForwardedTokenRefused => {
                "the owner token came with a forwarding header or a non-loopback host"
            }
        }
    }

    const fn refusal(self) -> bool {
        match self {
            Self::LoginCodeRefused
            | Self::LoginCodeRateLimited
            | Self::ExchangeRefused
            | Self::ExchangeRateLimited
            | Self::BearerRejected
            | Self::ForwardedTokenRefused => true,
            Self::LoginCodeMinted | Self::ExchangeSucceeded | Self::Logout | Self::Rotated => false,
        }
    }
}

#[derive(Debug)]
struct Window {
    start: Instant,
    suppressed: u64,
}

type Windows = HashMap<(AuthEvent, Option<IpAddr>), Window>;

#[derive(Debug, Default)]
pub(crate) struct AuthLog {
    windows: Arc<Mutex<Windows>>,
    flusher: Mutex<Option<JoinHandle<()>>>,
}

fn peer_text(peer: Option<IpAddr>) -> String {
    peer.map_or_else(|| "unknown".to_owned(), |ip| ip.to_string())
}

fn log_summaries(closed: Vec<((AuthEvent, Option<IpAddr>), u64)>) {
    for ((event, peer), suppressed) in closed {
        tracing::warn!(
            event = event.name(),
            peer = %peer_text(peer),
            suppressed,
            "auth event repeated: {suppressed} more in the last 60s"
        );
    }
}

fn drain(windows: &Mutex<Windows>, keep: impl Fn(&Window) -> bool) {
    let mut closed = Vec::new();
    windows.lock().retain(|key, window| {
        let keep = keep(window);
        if !keep && window.suppressed > 0 {
            closed.push((*key, window.suppressed));
        }
        keep
    });
    log_summaries(closed);
}

async fn flush_when_windows_close(windows: Arc<Mutex<Windows>>) {
    let mut tick = tokio::time::interval(FLUSH_TICK);
    loop {
        tick.tick().await;
        let now = Instant::now();
        drain(&windows, |window| {
            now.saturating_duration_since(window.start) < WINDOW
        });
    }
}

impl AuthLog {
    pub(crate) fn emit(&self, event: AuthEvent, peer: Option<IpAddr>) {
        self.emit_at(event, peer, Instant::now());
    }

    pub(crate) fn emit_at(&self, event: AuthEvent, peer: Option<IpAddr>, now: Instant) {
        let (name, reason) = (event.name(), event.reason());
        let text = peer_text(peer);
        drain(&self.windows, |window| {
            now.saturating_duration_since(window.start) < WINDOW
        });
        if !event.refusal() {
            tracing::info!(event = name, reason, peer = %text, "auth event");
            return;
        }
        let first = {
            let mut windows = self.windows.lock();
            if let Some(window) = windows.get_mut(&(event, peer)) {
                window.suppressed += 1;
                false
            } else if windows.len() < REFUSAL_LOG_CAP {
                windows.insert(
                    (event, peer),
                    Window {
                        start: now,
                        suppressed: 0,
                    },
                );
                true
            } else {
                false
            }
        };
        if first {
            tracing::warn!(event = name, reason, peer = %text, "auth event");
            self.ensure_flusher();
        }
    }

    fn ensure_flusher(&self) {
        let mut flusher = self.flusher.lock();
        if flusher.is_some() {
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let windows = Arc::clone(&self.windows);
            *flusher = Some(runtime.spawn(flush_when_windows_close(windows)));
        }
    }

    pub(crate) fn close(&self) {
        let flusher = self.flusher.lock().take();
        if let Some(flusher) = flusher {
            flusher.abort();
        }
        drain(&self.windows, |_| false);
    }
}

impl Drop for AuthLog {
    fn drop(&mut self) {
        if let Some(flusher) = self.flusher.get_mut().take() {
            flusher.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;
    use std::time::Duration;

    use parking_lot::Mutex;
    use tokio::time::Instant;

    use super::{AuthEvent, AuthLog};

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn a_brute_force_run_logs_the_first_refusal_then_one_summary_per_window() {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(captured.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let log = AuthLog::default();
        let peer = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let start = Instant::now();
        let lines = || String::from_utf8_lossy(&captured.0.lock()).into_owned();
        for second in 0..30 {
            log.emit_at(
                AuthEvent::ExchangeRefused,
                peer,
                start + Duration::from_secs(second),
            );
        }
        assert_eq!(lines().matches("exchange_refused").count(), 1);
        for second in 61..100 {
            log.emit_at(
                AuthEvent::ExchangeRefused,
                peer,
                start + Duration::from_secs(second),
            );
        }
        let text = lines();
        assert_eq!(text.matches("exchange_refused").count(), 3, "{text}");
        assert!(text.contains("29 more in the last 60s"), "{text}");
        log.emit_at(
            AuthEvent::ExchangeRefused,
            peer,
            start + Duration::from_secs(400),
        );
        let text = lines();
        assert!(text.contains("38 more in the last 60s"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_followed_by_silence_still_logs_its_count() {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(captured.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let log = AuthLog::default();
        let peer = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));
        for _ in 0..8 {
            log.emit(AuthEvent::ExchangeRefused, peer);
        }
        let lines = || String::from_utf8_lossy(&captured.0.lock()).into_owned();
        assert!(!lines().contains("more in the last"), "{}", lines());
        tokio::time::advance(Duration::from_secs(70)).await;
        tokio::task::yield_now().await;
        let text = lines();
        assert!(text.contains("7 more in the last 60s"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn close_flushes_pending_counts_and_stops_the_timer() {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(captured.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let log = AuthLog::default();
        for _ in 0..4 {
            log.emit(AuthEvent::BearerRejected, None);
        }
        log.close();
        let text = String::from_utf8_lossy(&captured.0.lock()).into_owned();
        assert!(text.contains("3 more in the last 60s"), "{text}");
        assert!(log.flusher.lock().is_none());
    }
}
