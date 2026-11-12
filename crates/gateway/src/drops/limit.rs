use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::time::Instant;

use crate::tenancy::UserId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub limit: u32,
    pub window: Duration,
}

impl Rate {
    #[must_use]
    pub const fn per(limit: u32, window: Duration) -> Self {
        Self { limit, window }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rates {
    pub downloads_per_ip: Rate,
    pub misses_per_ip: Rate,
    pub uploads_per_user: Rate,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 143: the default rates are the rivethub.cloud drop limits, not the derive's zeros"
    )
)]
impl Default for Rates {
    fn default() -> Self {
        const MINUTE: Duration = Duration::from_secs(60);
        Self {
            downloads_per_ip: Rate::per(20, MINUTE),
            misses_per_ip: Rate::per(10, MINUTE * 10),
            uploads_per_user: Rate::per(30, MINUTE * 60),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetryAfter(pub(crate) Duration);

impl RetryAfter {
    pub(crate) fn seconds(self) -> u64 {
        (self.0.as_secs() + u64::from(self.0.subsec_nanos() > 0)).max(1)
    }
}

#[derive(Debug)]
struct Window {
    count: u32,
    resets_at: Instant,
}

#[derive(Debug)]
struct FixedWindows<K> {
    rate: Rate,
    windows: Mutex<HashMap<K, Window>>,
}

impl<K: Hash + Eq + Clone> FixedWindows<K> {
    fn new(rate: Rate) -> Self {
        Self {
            rate,
            windows: Mutex::new(HashMap::new()),
        }
    }

    fn hit(&self, key: &K, now: Instant) -> Result<(), RetryAfter> {
        let mut windows = self.windows.lock();
        let window = windows.entry(key.clone()).or_insert(Window {
            count: 0,
            resets_at: now + self.rate.window,
        });
        if now >= window.resets_at {
            *window = Window {
                count: 0,
                resets_at: now + self.rate.window,
            };
        }
        let admitted = if window.count >= self.rate.limit {
            Err(RetryAfter(window.resets_at - now))
        } else {
            window.count += 1;
            Ok(())
        };
        drop(windows);
        admitted
    }

    fn full(&self, key: &K, now: Instant) -> Result<(), RetryAfter> {
        match self.windows.lock().get(key) {
            Some(window) if now < window.resets_at && window.count >= self.rate.limit => {
                Err(RetryAfter(window.resets_at - now))
            }
            Some(_) | None => Ok(()),
        }
    }

    fn prune(&self, now: Instant) {
        self.windows
            .lock()
            .retain(|_, window| now < window.resets_at);
    }
}

#[derive(Debug)]
pub(crate) struct Limits {
    downloads: FixedWindows<IpAddr>,
    misses: FixedWindows<IpAddr>,
    uploads: FixedWindows<UserId>,
}

impl Limits {
    pub(crate) fn new(rates: Rates) -> Self {
        Self {
            downloads: FixedWindows::new(rates.downloads_per_ip),
            misses: FixedWindows::new(rates.misses_per_ip),
            uploads: FixedWindows::new(rates.uploads_per_user),
        }
    }

    pub(crate) fn download(&self, ip: IpAddr) -> Result<(), RetryAfter> {
        let now = Instant::now();
        self.misses.full(&ip, now)?;
        self.downloads.hit(&ip, now)
    }

    pub(crate) fn missed(&self, ip: IpAddr) {
        if self.misses.hit(&ip, Instant::now()).is_err() {
            tracing::debug!("drop misses from one address are past their limit");
        }
    }

    pub(crate) fn upload(&self, user: &UserId) -> Result<(), RetryAfter> {
        self.uploads.hit(user, Instant::now())
    }

    pub(crate) fn prune(&self) {
        let now = Instant::now();
        self.downloads.prune(now);
        self.misses.prune(now);
        self.uploads.prune(now);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::Instant;

    use super::{FixedWindows, Rate};

    #[test]
    fn a_window_admits_its_limit_then_refuses_until_it_resets() {
        let windows = FixedWindows::new(Rate::per(2, Duration::from_secs(60)));
        let start = Instant::now();
        assert!(windows.hit(&"ip", start).is_ok());
        assert!(windows.full(&"ip", start).is_ok());
        assert!(windows.hit(&"ip", start).is_ok());
        let refused = windows.hit(&"ip", start + Duration::from_secs(20));
        assert_eq!(refused.map_err(super::RetryAfter::seconds), Err(40));
        assert!(windows.full(&"ip", start).is_err());
        assert!(windows.hit(&"other", start).is_ok());
        assert!(windows.hit(&"ip", start + Duration::from_secs(60)).is_ok());
        windows.prune(start + Duration::from_secs(200));
        assert!(windows.windows.lock().is_empty());
    }
}
