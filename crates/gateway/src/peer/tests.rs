use std::net::{IpAddr, SocketAddr};

use axum::http::{HeaderMap, HeaderValue};
use config::model::TrustedProxy;

use super::Peer;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn trusted(entries: &[&str]) -> Result<Vec<TrustedProxy>, String> {
    entries
        .iter()
        .map(|entry| TrustedProxy::try_from((*entry).to_owned()))
        .collect()
}

fn peer(remote: &str, headers: &[(&'static str, &str)], proxies: &[&str]) -> TestResult<Peer> {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        map.append(*name, value.parse()?);
    }
    Ok(Peer::of(
        remote.parse::<SocketAddr>()?,
        &map,
        &trusted(proxies)?,
    ))
}

fn ip(text: &str) -> TestResult<IpAddr> {
    Ok(text.parse()?)
}

#[test]
fn only_an_unforwarded_loopback_peer_is_direct_local() -> TestResult {
    assert_eq!(
        peer("127.0.0.1:9", &[], &[])?,
        Peer::DirectLocal {
            address: ip("127.0.0.1")?,
        }
    );
    assert_eq!(
        peer("[::ffff:127.0.0.1]:9", &[], &[])?,
        Peer::DirectLocal {
            address: ip("127.0.0.1")?,
        }
    );
    for header in [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-forwarded-port",
        "x-real-ip",
    ] {
        assert_eq!(
            peer("127.0.0.1:9", &[(header, "203.0.113.9")], &[])?,
            Peer::Remote(ip("127.0.0.1")?),
            "{header}"
        );
    }
    assert_eq!(
        peer("192.0.2.7:9", &[], &[])?,
        Peer::Remote(ip("192.0.2.7")?)
    );
    Ok(())
}

#[test]
fn a_trusted_proxy_names_the_rightmost_untrusted_hop() -> TestResult {
    let proxies = ["127.0.0.1/32", "198.51.100.0/24"];
    let forwarded = |chain: &'static str| -> TestResult<Peer> {
        peer(
            "127.0.0.1:9",
            &[("x-forwarded-for", chain), ("x-forwarded-proto", "https")],
            &proxies,
        )
    };
    let client = |text: &str| -> TestResult<Peer> {
        Ok(Peer::Proxied {
            client: ip(text)?,
            https: true,
        })
    };
    assert_eq!(forwarded("203.0.113.9")?, client("203.0.113.9")?);
    assert_eq!(
        forwarded("192.0.2.1, 203.0.113.9, 198.51.100.3")?,
        client("203.0.113.9")?,
        "a spoofed leftmost hop is ignored"
    );
    assert_eq!(forwarded("198.51.100.3")?, client("127.0.0.1")?);
    assert_eq!(forwarded("not-an-ip")?, client("127.0.0.1")?);
    Ok(())
}

#[test]
fn the_forwarded_walk_skips_what_it_cannot_read() -> TestResult {
    let proxies = ["127.0.0.1/32", "198.51.100.0/24"];
    let walked = |headers: &[(&'static str, &str)]| -> TestResult<IpAddr> {
        Ok(peer("127.0.0.1:9", headers, &proxies)?.client())
    };
    let xff = |chain: &'static str| walked(&[("x-forwarded-for", chain)]);
    assert_eq!(xff("203.0.113.9:5678")?, ip("203.0.113.9")?);
    assert_eq!(xff("[2001:db8::7]:80")?, ip("2001:db8::7")?);
    assert_eq!(xff("[2001:db8::7]")?, ip("2001:db8::7")?);
    assert_eq!(xff("[::ffff:203.0.113.9]:80")?, ip("203.0.113.9")?);
    assert_eq!(
        xff("203.0.113.9, unknown, 198.51.100.3")?,
        ip("203.0.113.9")?,
        "an unparseable hop is skipped, not the end of the walk"
    );
    assert_eq!(
        xff("203.0.113.9, 198.51.100.3:443, garbage")?,
        ip("203.0.113.9")?
    );
    assert_eq!(xff("unknown, garbage")?, ip("127.0.0.1")?);
    let mut opaque = HeaderMap::new();
    opaque.append(
        "x-forwarded-for",
        HeaderValue::from_bytes(b"203.0.113.9\xff")?,
    );
    let alone = Peer::of(
        "127.0.0.1:9".parse::<SocketAddr>()?,
        &opaque,
        &trusted(&proxies)?,
    );
    assert_eq!(
        alone.client(),
        ip("127.0.0.1")?,
        "non-UTF-8 has no usable hop"
    );
    opaque.append("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
    let beside = Peer::of(
        "127.0.0.1:9".parse::<SocketAddr>()?,
        &opaque,
        &trusted(&proxies)?,
    );
    assert_eq!(beside.client(), ip("203.0.113.9")?);
    Ok(())
}

#[test]
fn a_proxied_request_is_secure_only_when_the_proxy_says_https() -> TestResult {
    let proxies = ["127.0.0.1"];
    let proto = |values: &[&'static str]| -> TestResult<bool> {
        let headers: Vec<(&'static str, &str)> = values
            .iter()
            .map(|value| ("x-forwarded-proto", *value))
            .collect();
        Ok(peer("127.0.0.1:9", &headers, &proxies)?.secure(true))
    };
    assert!(proto(&["https"])?);
    assert!(proto(&["HTTPS"])?);
    assert!(!proto(&["http"])?);
    assert!(!proto(&[])?);
    assert!(!proto(&["https", "http"])?);
    assert!(!proto(&["https, http"])?);
    assert!(peer("192.0.2.7:9", &[], &proxies)?.secure(true));
    assert!(!peer("192.0.2.7:9", &[], &proxies)?.secure(false));
    Ok(())
}
