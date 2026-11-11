use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::extract::Request;
use axum::extract::State;
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use governor::Quota;
use governor::RateLimiter;
use governor::clock::Clock as _;
use governor::clock::DefaultClock;
use governor::state::InMemoryState;
use governor::state::NotKeyed;
use governor::state::keyed::DefaultKeyedStateStore;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest as _;
use sha2::Sha256;
use subtle::ConstantTimeEq as _;
use tokio::time::Instant;
use tower_sessions::Expiry;
use tower_sessions::MemoryStore;
use tower_sessions::Session;
use tower_sessions::SessionManagerLayer;
use tower_sessions::cookie::SameSite;
use tower_sessions::cookie::time;

pub(crate) const SESSION_COOKIE: &str = "__Host-rh_session";
pub(crate) const CSRF_HEADER: &str = "x-rh-csrf";
pub(crate) const LOGIN_CODE_TTL: Duration = Duration::from_secs(60);
const LOGIN_KEY: &str = "login";
const MAX_LIVE_CODES: usize = 16;
const SECRET_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthPolicy {
    pub idle: Duration,
    pub absolute: Duration,
    pub fresh: Duration,
    pub attempts_per_minute: NonZeroU32,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "SS-15a: the defaults are the decided lifetimes (7 d idle, 30 d absolute, 5 min fresh), not zeros"
    )
)]
impl Default for AuthPolicy {
    fn default() -> Self {
        Self {
            idle: Duration::from_hours(7 * 24),
            absolute: Duration::from_hours(30 * 24),
            fresh: Duration::from_secs(300),
            attempts_per_minute: NonZeroU32::MIN.saturating_add(9),
        }
    }
}

impl AuthPolicy {
    #[must_use]
    pub fn from_config(config: &config::Config) -> Self {
        let mut policy = Self::default();
        if let Some(secs) = config
            .gateway
            .as_ref()
            .and_then(|section| section.fresh_login_secs)
        {
            policy.fresh = Duration::from_secs(secs);
        }
        policy
    }
}

pub(crate) enum MintError {
    Full(Duration),
    Random,
}

#[derive(Debug, Default)]
pub(crate) struct LoginCodes {
    live: Mutex<Vec<([u8; 32], Instant)>>,
}

impl LoginCodes {
    pub(crate) fn mint(&self) -> Result<String, MintError> {
        let now = Instant::now();
        let mut live = self.live.lock();
        live.retain(|(_, deadline)| *deadline > now);
        if live.len() >= MAX_LIVE_CODES {
            let soonest = live.iter().map(|(_, deadline)| *deadline).min();
            drop(live);
            return Err(MintError::Full(
                soonest.map_or(LOGIN_CODE_TTL, |at| at.saturating_duration_since(now)),
            ));
        }
        let Some(code) = random_secret() else {
            drop(live);
            return Err(MintError::Random);
        };
        live.push((Sha256::digest(code.as_bytes()).into(), now + LOGIN_CODE_TTL));
        drop(live);
        Ok(code)
    }

    pub(crate) fn redeem(&self, presented: &str) -> bool {
        let digest: [u8; 32] = Sha256::digest(presented.as_bytes()).into();
        let now = Instant::now();
        let mut live = self.live.lock();
        live.retain(|(_, deadline)| *deadline > now);
        let mut hit = None;
        for (index, (known, _)) in live.iter().enumerate() {
            if bool::from(known.ct_eq(&digest)) {
                hit = Some(index);
            }
        }
        let redeemed = hit.map(|index| live.swap_remove(index)).is_some();
        drop(live);
        redeemed
    }

    fn clear(&self) {
        self.live.lock().clear();
    }
}

pub(crate) fn random_secret() -> Option<String> {
    let mut bytes = [0_u8; SECRET_BYTES];
    getrandom::fill(&mut bytes).ok()?;
    Some(URL_SAFE_NO_PAD.encode(bytes))
}

type PerPeer = RateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>;
type Overall = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

struct Attempts {
    per_peer: PerPeer,
    overall: Overall,
}

impl Attempts {
    fn new(per_minute: NonZeroU32) -> Self {
        Self {
            per_peer: RateLimiter::keyed(Quota::per_minute(per_minute)),
            overall: RateLimiter::direct(Quota::per_minute(
                per_minute.saturating_mul(NonZeroU32::MIN.saturating_add(1)),
            )),
        }
    }

    fn admit(&self, peer: IpAddr) -> Result<(), Duration> {
        let clock = DefaultClock::default();
        self.per_peer
            .check_key(&peer)
            .map_err(|early| early.wait_time_from(clock.now()))?;
        self.overall
            .check()
            .map_err(|early| early.wait_time_from(clock.now()))
    }
}

#[derive(Serialize, Deserialize)]
struct Login {
    issued_at: i64,
    auth_time: i64,
    csrf: String,
    epoch: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BrowserLogin {
    pub(crate) csrf: [u8; 32],
    pub(crate) auth_time: i64,
}

pub(crate) struct Browser {
    policy: AuthPolicy,
    store: MemoryStore,
    pub(crate) codes: LoginCodes,
    attempts: Attempts,
    epoch: AtomicU64,
}

impl Browser {
    pub(crate) fn new(policy: AuthPolicy) -> Self {
        Self {
            policy,
            store: MemoryStore::default(),
            codes: LoginCodes::default(),
            attempts: Attempts::new(policy.attempts_per_minute),
            epoch: AtomicU64::new(0),
        }
    }

    pub(crate) fn policy(&self) -> &AuthPolicy {
        &self.policy
    }

    pub(crate) fn layer(&self) -> SessionManagerLayer<MemoryStore> {
        let idle = time::Duration::try_from(self.policy.idle).unwrap_or(time::Duration::MAX);
        SessionManagerLayer::new(self.store.clone())
            .with_name(SESSION_COOKIE)
            .with_secure(true)
            .with_http_only(true)
            .with_same_site(SameSite::Strict)
            .with_path("/")
            .with_always_save(true)
            .with_expiry(Expiry::OnInactivity(idle))
    }

    pub(crate) fn admit(&self, peer: IpAddr) -> Result<(), Duration> {
        self.attempts.admit(peer)
    }

    pub(crate) fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.codes.clear();
    }

    pub(crate) async fn begin(&self, session: &Session) -> Option<String> {
        let csrf = random_secret()?;
        let now = chrono::Utc::now().timestamp();
        let login = Login {
            issued_at: now,
            auth_time: now,
            csrf: csrf.clone(),
            epoch: self.epoch.load(Ordering::Acquire),
        };
        session.cycle_id().await.ok()?;
        session.insert(LOGIN_KEY, login).await.ok()?;
        Some(csrf)
    }

    fn accepts(&self, login: &Login) -> bool {
        let age = chrono::Utc::now()
            .timestamp()
            .saturating_sub(login.issued_at);
        login.epoch == self.epoch.load(Ordering::Acquire)
            && u64::try_from(age).is_ok_and(|age| age < self.policy.absolute.as_secs())
    }
}

pub(crate) async fn resolve(
    State(gateway): State<std::sync::Arc<crate::Gateway>>,
    session: Session,
    mut request: Request,
    next: Next,
) -> Response {
    let has_cookie = request.headers().contains_key(header::COOKIE);
    if has_cookie && !request.headers().contains_key(header::AUTHORIZATION) {
        match session.get::<Login>(LOGIN_KEY).await {
            Ok(Some(login)) if gateway.browser().accepts(&login) => {
                let digest = Sha256::digest(login.csrf.as_bytes()).into();
                request.extensions_mut().insert(BrowserLogin {
                    csrf: digest,
                    auth_time: login.auth_time,
                });
            }
            Ok(Some(_)) => {
                if session.flush().await.is_err() {
                    tracing::warn!("an expired browser session could not be removed");
                }
            }
            Ok(None) => {}
            Err(_) => tracing::warn!("the browser session store could not be read"),
        }
    }
    next.run(request).await
}

pub(crate) fn peer_ip(parts: &axum::http::request::Parts) -> Option<IpAddr> {
    parts
        .extensions
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ConnectInfo(remote)| remote.ip())
}
