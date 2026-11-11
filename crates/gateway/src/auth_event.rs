use std::collections::HashSet;
use std::net::IpAddr;

use parking_lot::Mutex;

const REFUSAL_LOG_CAP: usize = 100;

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

#[derive(Debug, Default)]
pub(crate) struct AuthLog {
    seen: Mutex<HashSet<(AuthEvent, Option<IpAddr>)>>,
}

impl AuthLog {
    pub(crate) fn emit(&self, event: AuthEvent, peer: Option<IpAddr>) {
        let (name, reason) = (event.name(), event.reason());
        let peer = peer.map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
        if !event.refusal() {
            tracing::info!(event = name, reason, peer = %peer, "auth event");
            return;
        }
        let first = {
            let mut seen = self.seen.lock();
            seen.len() < REFUSAL_LOG_CAP && seen.insert((event, peer.parse().ok()))
        };
        if first {
            tracing::warn!(event = name, reason, peer = %peer, "auth event");
        }
    }
}
