use std::path::Path;

use gateway::server::BindSource;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn config(text: &str) -> Result<config::Config, serde_yaml_ng::Error> {
    serde_yaml_ng::from_str(text)
}

fn environment(pairs: &[(&str, &str)]) -> config::Environment {
    config::Environment::from_vars(pairs.iter().copied())
}

fn resolve(
    text: &str,
    env: &config::Environment,
    node: &str,
    shared: &Path,
) -> Result<BindSource, serde_yaml_ng::Error> {
    Ok(BindSource::resolve(&config(text)?, env, node, shared))
}

fn write(path: &Path, bytes: &[u8]) -> TestResult {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)?;
    Ok(())
}

#[test]
fn config_paths_beat_environment_and_issued_files() -> TestResult {
    let root = tempfile::tempdir()?;
    let configured_cert = root.path().join("config.crt");
    let configured_key = root.path().join("config.key");
    let configured_ca = root.path().join("config-ca.pem");
    write(&configured_cert, b"config-cert")?;
    write(&configured_key, b"config-key")?;
    write(&configured_ca, b"config-ca")?;
    let shared = root.path().join("shared");
    write(&shared.join("rivet-ca/issued/node.crt"), b"issued-cert")?;
    write(&shared.join("rivet-ca/issued/node.key"), b"issued-key")?;
    let env = environment(&[
        ("RIVETHUB_TLS_CERT", "/env/node.crt"),
        ("RIVETHUB_TLS_KEY", "/env/node.key"),
        ("RIVETHUB_TLS_CA", "/env/ca.pem"),
        ("RIVETOS_DEN_TLS_CERT", "/legacy/node.crt"),
        ("RIVETOS_DEN_TLS_KEY", "/legacy/node.key"),
        ("RIVETOS_DEN_TLS_CA", "/legacy/ca.pem"),
    ]);
    let text = format!(
        "gateway:\n  host: 0.0.0.0\n  tls_cert: {}\n  tls_key: {}\n",
        configured_cert.display(),
        configured_key.display()
    );
    let source = resolve(&text, &env, "node", &shared)?;
    assert_eq!(source.tls_cert.as_deref(), Some(configured_cert.as_path()));
    assert_eq!(source.tls_key.as_deref(), Some(configured_key.as_path()));
    assert_eq!(source.notices, Vec::<String>::new());
    Ok(())
}

#[test]
fn current_environment_beats_legacy_names_and_issued_files() -> TestResult {
    let root = tempfile::tempdir()?;
    let cert = root.path().join("env.crt");
    let key = root.path().join("env.key");
    let ca = root.path().join("env-ca.pem");
    write(&cert, b"env-cert")?;
    write(&key, b"env-key")?;
    write(&ca, b"env-ca")?;
    let shared = root.path().join("shared");
    write(&shared.join("rivet-ca/issued/node.crt"), b"issued-cert")?;
    write(&shared.join("rivet-ca/issued/node.key"), b"issued-key")?;
    let cert_text = cert.to_str().ok_or("utf-8 cert")?;
    let key_text = key.to_str().ok_or("utf-8 key")?;
    let ca_text = ca.to_str().ok_or("utf-8 ca")?;
    let env = environment(&[
        ("RIVETHUB_TLS_CERT", cert_text),
        ("RIVETHUB_TLS_KEY", key_text),
        ("RIVETHUB_TLS_CA", ca_text),
        ("RIVETOS_DEN_TLS_CERT", "/legacy/node.crt"),
        ("RIVETOS_DEN_TLS_KEY", "/legacy/node.key"),
        ("RIVETOS_DEN_TLS_CA", "/legacy/ca.pem"),
    ]);
    let source = resolve("gateway:\n  host: 0.0.0.0\n", &env, "node", &shared)?;
    assert_eq!(source.tls_cert.as_deref(), Some(cert.as_path()));
    assert_eq!(source.tls_key.as_deref(), Some(key.as_path()));
    assert_eq!(source.notices, Vec::<String>::new());
    Ok(())
}

#[test]
fn legacy_environment_is_used_only_when_the_current_name_is_unset() -> TestResult {
    let root = tempfile::tempdir()?;
    let cert = root.path().join("legacy.crt");
    let key = root.path().join("legacy.key");
    write(&cert, b"legacy-cert")?;
    write(&key, b"legacy-key")?;
    let cert_text = cert.to_str().ok_or("utf-8 cert")?;
    let key_text = key.to_str().ok_or("utf-8 key")?;
    let env = environment(&[
        ("RIVETHUB_TLS_CERT", "  "),
        ("RIVETOS_DEN_TLS_CERT", cert_text),
        ("RIVETOS_DEN_TLS_KEY", key_text),
        ("RIVET_CA_CHAIN", "/legacy/ca.pem"),
    ]);
    let source = resolve("gateway:\n  host: 0.0.0.0\n", &env, "node", root.path())?;
    assert_eq!(source.tls_cert.as_deref(), Some(cert.as_path()));
    assert_eq!(source.tls_key.as_deref(), Some(key.as_path()));
    assert_eq!(
        source.notices,
        vec![
            env.legacy_notice("RIVETHUB_TLS_CERT", "RIVETOS_DEN_TLS_CERT")
                .ok_or("cert notice")?,
            env.legacy_notice("RIVETHUB_TLS_KEY", "RIVETOS_DEN_TLS_KEY")
                .ok_or("key notice")?,
        ]
    );
    Ok(())
}

#[test]
fn issued_pair_is_used_only_when_both_files_stay_inside_the_directory() -> TestResult {
    let root = tempfile::tempdir()?;
    let shared = root.path().join("shared");
    let issued = shared.join("rivet-ca/issued");
    write(&issued.join("node.crt"), b"issued-cert")?;
    write(&issued.join("node.key"), b"issued-key")?;
    let env = environment(&[]);
    let source = resolve("gateway:\n  host: 0.0.0.0\n", &env, "node", &shared)?;
    assert_eq!(
        source.tls_cert.as_deref(),
        Some(issued.join("node.crt").canonicalize()?.as_path())
    );
    assert_eq!(
        source.tls_key.as_deref(),
        Some(issued.join("node.key").canonicalize()?.as_path())
    );

    let cert_only = root.path().join("cert-only");
    write(&cert_only.join("rivet-ca/issued/node.crt"), b"only-cert")?;
    let missing = resolve("gateway:\n  host: 0.0.0.0\n", &env, "node", &cert_only)?;
    assert!(missing.tls_cert.is_none());
    assert!(missing.tls_key.is_none());

    let outside = root.path().join("outside.crt");
    write(&outside, b"escaped")?;
    let linked = root.path().join("linked");
    let linked_issued = linked.join("rivet-ca/issued");
    std::fs::create_dir_all(&linked_issued)?;
    std::os::unix::fs::symlink(&outside, linked_issued.join("node.crt"))?;
    write(&linked_issued.join("node.key"), b"inside-key")?;
    let escaped = resolve("gateway:\n  host: 0.0.0.0\n", &env, "node", &linked)?;
    assert!(escaped.tls_cert.is_none());
    assert!(escaped.tls_key.is_none());

    write(&issued.join("foo/bar.crt"), b"nested-cert")?;
    write(&issued.join("foo/bar.key"), b"nested-key")?;
    for node in ["..", "foo/bar", "", ".", "node/../node"] {
        let refused = resolve("gateway:\n  host: 0.0.0.0\n", &env, node, &shared)?;
        assert!(
            refused.tls_cert.is_none() && refused.tls_key.is_none(),
            "{node}"
        );
    }
    Ok(())
}

#[test]
fn configured_certificate_can_take_its_key_from_the_issued_pair() -> TestResult {
    let root = tempfile::tempdir()?;
    let shared = root.path().join("shared");
    let issued = shared.join("rivet-ca/issued");
    write(&issued.join("node.crt"), b"issued-cert")?;
    write(&issued.join("node.key"), b"issued-key")?;
    let cert = root.path().join("config.crt");
    write(&cert, b"config-cert")?;
    let text = format!("gateway:\n  tls_cert: {}\n", cert.display());
    let source = resolve(&text, &environment(&[]), "node", &shared)?;
    assert_eq!(source.tls_cert.as_deref(), Some(cert.as_path()));
    assert_eq!(
        source.tls_key.as_deref(),
        Some(issued.join("node.key").canonicalize()?.as_path())
    );
    Ok(())
}

#[test]
fn an_absent_gateway_section_keeps_the_loopback_default() -> TestResult {
    let root = tempfile::tempdir()?;
    let source = resolve("", &environment(&[]), "node", root.path())?;
    assert_eq!(source.host, "127.0.0.1");
    assert_eq!(source.port, 5174);
    assert!(source.tls_cert.is_none());
    Ok(())
}
