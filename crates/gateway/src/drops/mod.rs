mod limit;
pub(crate) mod store;

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::http::Method;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::TimeDelta;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

pub(crate) use limit::RetryAfter;
pub use limit::{Rate, Rates};

use crate::routes::join_task;
use crate::tenancy::UserId;

pub(crate) const AGE_HEADER: &[u8] = b"age-encryption.org/v1\n";
pub const UPLOAD_DEADLINE: Duration = Duration::from_mins(10);
pub(crate) const UPLOAD_BODY_DEADLINE: Duration = Duration::from_mins(11);
pub(crate) const SEND_DEADLINE: Duration = Duration::from_mins(15);
const SWEEP_EVERY: Duration = Duration::from_mins(10);
pub const ORPHAN_AGE: Duration = Duration::from_hours(1);
pub const DROP_TTL: TimeDelta = TimeDelta::days(7);
const DROPS_PATH: &str = "/api/drops";

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct DropId(String);

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 143: a drop id is the download capability, so Debug never prints it"
    )
)]
impl std::fmt::Debug for DropId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DropId(..)")
    }
}

impl DropId {
    pub const LEN: usize = 22;

    fn generate() -> std::io::Result<Self> {
        let mut raw = [0_u8; 16];
        getrandom::fill(&mut raw).map_err(std::io::Error::other)?;
        Ok(Self(URL_SAFE_NO_PAD.encode(raw)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
#[error("not a drop id")]
pub struct NotADropId;

impl std::str::FromStr for DropId {
    type Err = NotADropId;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let valid = text.len() == Self::LEN
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if valid {
            Ok(Self(text.to_owned()))
        } else {
            Err(NotADropId)
        }
    }
}

#[derive(Debug, Clone)]
pub struct DropsConfig {
    pub root: PathBuf,
    pub max_bytes: u64,
    pub per_user_drops: u32,
    pub per_user_bytes: u64,
    pub ttl: TimeDelta,
    pub rates: Rates,
}

impl DropsConfig {
    #[must_use]
    pub fn new(root: PathBuf, section: &config::model::DropsSection) -> Self {
        Self {
            root,
            max_bytes: section.max_bytes,
            per_user_drops: section.per_user_drops,
            per_user_bytes: section.per_user_bytes,
            ttl: DROP_TTL,
            rates: Rates::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("the drop root {path} {rule}")]
    Unsafe { path: PathBuf, rule: &'static str },
    #[error("the drop root {path} could not be prepared: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug)]
pub struct Drops {
    store: store::Store,
    limits: limit::Limits,
    max_bytes: u64,
}

impl Drops {
    pub fn open(config: &DropsConfig) -> Result<Self, OpenError> {
        Ok(Self {
            store: store::Store::open(config)?,
            limits: limit::Limits::new(config.rates),
            max_bytes: config.max_bytes,
        })
    }

    pub(crate) const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    pub(crate) const fn store(&self) -> &store::Store {
        &self.store
    }

    pub(crate) fn download(&self, ip: IpAddr) -> Result<(), RetryAfter> {
        self.limits.download(ip)
    }

    pub(crate) fn missed(&self, ip: IpAddr) {
        self.limits.missed(ip);
    }

    pub(crate) fn upload(&self, user: &UserId) -> Result<(), RetryAfter> {
        self.limits.upload(user)
    }

    pub async fn sweep(self: &Arc<Self>, orphan_age: Duration) -> Result<usize, std::io::Error> {
        let drops = Arc::clone(self);
        let swept = tokio::task::spawn_blocking(move || {
            drops.limits.prune();
            drops.store.sweep(orphan_age)
        })
        .await;
        join_task(swept).map_err(std::io::Error::other)?
    }

    pub fn spawn_sweeper(self: &Arc<Self>, tracker: &TaskTracker, shutdown: CancellationToken) {
        let drops = Arc::clone(self);
        tracker.spawn(async move {
            let mut beat = tokio::time::interval(SWEEP_EVERY);
            beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    _ = beat.tick() => {}
                }
                if let Err(error) = drops.sweep(ORPHAN_AGE).await {
                    tracing::warn!(kind = ?error.kind(), "drop sweep failed");
                }
            }
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DropRoute {
    Collection,
    Item,
}

impl DropRoute {
    fn of(path: &str) -> Option<Self> {
        let rest = path.strip_prefix(DROPS_PATH)?;
        if rest.is_empty() {
            return Some(Self::Collection);
        }
        let id = rest.strip_prefix('/')?;
        (!id.is_empty() && !id.contains('/')).then_some(Self::Item)
    }
}

pub(crate) fn is_upload(method: &Method, path: &str) -> bool {
    method == Method::POST && DropRoute::of(path) == Some(DropRoute::Collection)
}

pub(crate) fn is_revoke(method: &Method, path: &str) -> bool {
    method == Method::DELETE && DropRoute::of(path) == Some(DropRoute::Item)
}

pub(crate) fn open_to_anyone(method: &Method, path: &str, secure: bool) -> bool {
    match DropRoute::of(path) {
        None => false,
        Some(_) if *method == Method::POST || *method == Method::DELETE => secure,
        Some(DropRoute::Collection | DropRoute::Item) => true,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Method;

    use super::{DropId, open_to_anyone};

    #[test]
    fn a_drop_id_is_22_url_safe_characters() {
        let id = DropId::generate().expect("random source");
        assert_eq!(id.as_str().len(), DropId::LEN);
        assert!(id.as_str().parse::<DropId>().is_ok());
        for bad in [
            "short",
            "../../etc/passwd/xxxxxxxxxx",
            "aaaaaaaaaaaaaaaaaaaaa.",
            "",
        ] {
            assert!(bad.parse::<DropId>().is_err(), "{bad}");
        }
        assert_eq!(format!("{id:?}"), "DropId(..)");
    }

    #[test]
    fn only_reads_reach_the_drop_box_without_tls() {
        let id = "/api/drops/AAAAAAAAAAAAAAAAAAAAAA";
        assert!(open_to_anyone(&Method::GET, id, false));
        assert!(!open_to_anyone(&Method::DELETE, id, false));
        assert!(!open_to_anyone(&Method::POST, "/api/drops", false));
        assert!(open_to_anyone(&Method::DELETE, id, true));
        assert!(open_to_anyone(&Method::POST, "/api/drops", true));
        assert!(!open_to_anyone(&Method::GET, "/api/dropsx", false));
        assert!(!open_to_anyone(&Method::GET, "/api/drops/a/b", false));
        assert!(!open_to_anyone(&Method::GET, "/api/memory/stats", true));
    }
}
