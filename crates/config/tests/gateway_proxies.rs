use std::net::IpAddr;

use config::{Profile, type_document, validate_document};

type Outcome = Result<(), Box<dyn std::error::Error>>;

fn document(text: &str) -> Result<serde_yaml_ng::Value, serde_yaml_ng::Error> {
    serde_yaml_ng::from_str(text)
}

fn proxies(entries: &str) -> Result<serde_yaml_ng::Value, serde_yaml_ng::Error> {
    document(&format!(
        "memory:\n  backend: sqlite\ngateway:\n  trusted_proxies: {entries}\n"
    ))
}

#[test]
fn no_proxy_is_trusted_by_default() -> Outcome {
    let yaml = document("memory:\n  backend: sqlite\ngateway:\n  port: 5174\n")?;
    let gateway = type_document(&yaml)?
        .config
        .gateway
        .ok_or("gateway section")?;
    assert_eq!(gateway.trusted_proxies, []);
    Ok(())
}

#[test]
fn a_trusted_proxy_is_an_address_or_a_cidr_block() -> Outcome {
    let yaml = proxies(r#"["127.0.0.1/32", "198.51.100.0/24", "::1", "2001:db8::/32"]"#)?;
    let result = validate_document(&yaml, Profile::Standalone);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let trusted = type_document(&yaml)?
        .config
        .gateway
        .ok_or("gateway section")?
        .trusted_proxies;
    let covered = |text: &str| -> Result<bool, std::net::AddrParseError> {
        let address: IpAddr = text.parse()?;
        Ok(trusted.iter().any(|proxy| proxy.contains(address)))
    };
    assert!(covered("127.0.0.1")?);
    assert!(
        covered("::ffff:127.0.0.1")?,
        "a mapped IPv4 peer is its IPv4 address"
    );
    assert!(!covered("127.0.0.2")?);
    assert!(covered("198.51.100.40")?);
    assert!(covered("::1")?);
    assert!(covered("2001:db8::7")?);
    assert!(!covered("192.0.2.7")?);
    Ok(())
}

#[test]
fn a_bad_trusted_proxy_is_refused_at_load() -> Outcome {
    for bad in [
        r#"["caddy"]"#,
        r#"["198.51.100.0/33"]"#,
        r#"["127.0.0.1", ""]"#,
    ] {
        let yaml = proxies(bad)?;
        assert!(type_document(&yaml).is_err(), "{bad} typed");
        let result = validate_document(&yaml, Profile::Standalone);
        assert!(
            result
                .errors
                .iter()
                .any(|finding| finding.path.starts_with("gateway.trusted_proxies")),
            "{bad}: {:?}",
            result.errors
        );
    }
    Ok(())
}

#[test]
fn a_mapped_ipv4_block_is_its_ipv4_block() -> Outcome {
    let yaml = proxies(r#"["::ffff:127.0.0.1/128", "::ffff:198.51.100.0/120"]"#)?;
    let trusted = type_document(&yaml)?
        .config
        .gateway
        .ok_or("gateway section")?
        .trusted_proxies;
    let unmapped = ["127.0.0.1/32", "198.51.100.0/24"]
        .map(|entry| config::model::TrustedProxy::try_from(entry.to_owned()));
    assert_eq!(
        trusted,
        unmapped.into_iter().collect::<Result<Vec<_>, _>>()?
    );
    for (text, covered) in [
        ("127.0.0.1", true),
        ("::ffff:127.0.0.1", true),
        ("198.51.100.40", true),
        ("127.0.0.2", false),
    ] {
        let address: IpAddr = text.parse()?;
        assert_eq!(
            trusted.iter().any(|proxy| proxy.contains(address)),
            covered,
            "{text}"
        );
    }
    Ok(())
}
