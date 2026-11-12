use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;
use config::model::TrustedProxy;

use crate::auth::{is_loopback_remote, names_a_forwarder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peer {
    DirectLocal { address: IpAddr },
    Proxied { client: IpAddr, https: bool },
    Remote(IpAddr),
}

impl Peer {
    #[must_use]
    pub fn of(remote: SocketAddr, headers: &HeaderMap, trusted: &[TrustedProxy]) -> Self {
        let address = remote.ip().to_canonical();
        if is_trusted(trusted, address) {
            return Self::Proxied {
                client: forwarded_client(headers, trusted).unwrap_or(address),
                https: forwarded_https(headers),
            };
        }
        if is_loopback_remote(remote) && !names_a_forwarder(headers) {
            Self::DirectLocal { address }
        } else {
            Self::Remote(address)
        }
    }

    #[must_use]
    pub const fn client(self) -> IpAddr {
        match self {
            Self::DirectLocal { address } | Self::Remote(address) => address,
            Self::Proxied { client, .. } => client,
        }
    }

    #[must_use]
    pub const fn secure(self, tls: bool) -> bool {
        match self {
            Self::DirectLocal { .. } => true,
            Self::Proxied { https, .. } => https,
            Self::Remote(_) => tls,
        }
    }
}

fn is_trusted(trusted: &[TrustedProxy], address: IpAddr) -> bool {
    trusted.iter().any(|proxy| proxy.contains(address))
}

fn forwarded_client(headers: &HeaderMap, trusted: &[TrustedProxy]) -> Option<IpAddr> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a stated rule, as note 143 of the contract has an X-Forwarded-For value that is not UTF-8 contribute no hops"
        )
    )]
    let values: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    values
        .iter()
        .flat_map(|value| value.split(','))
        .rev()
        .filter_map(hop_address)
        .find(|address| !is_trusted(trusted, *address))
}

fn hop_address(hop: &str) -> Option<IpAddr> {
    let hop = hop.trim();
    let bare = hop
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(hop);
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a stated rule, as note 143 of the contract skips an X-Forwarded-For hop that does not parse and walks on"
        )
    )]
    let address = bare
        .parse::<IpAddr>()
        .or_else(|_| hop.parse::<SocketAddr>().map(|socket| socket.ip()))
        .ok()?;
    Some(address.to_canonical())
}

fn forwarded_https(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all("x-forwarded-proto").iter();
    match (values.next(), values.next()) {
        (Some(value), None) => value
            .to_str()
            .is_ok_and(|proto| proto.trim().eq_ignore_ascii_case("https")),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
