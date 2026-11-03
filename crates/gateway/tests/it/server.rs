use std::path::PathBuf;

use gateway::server::BindError;
use gateway::server::BindSource;
use gateway::server::is_bind_loopback;
use gateway::tls::resolve_tls;
use serde::Deserialize;
use tempfile::tempdir;

fn masks() -> testkit::Masks {
    testkit::Masks {
        port: "5174".to_string(),
        hostname: Some("gold-host".to_string()),
        tmp: Some("/tmp/golden".to_string()),
    }
}

#[derive(Deserialize)]
struct Refusals {
    runs: std::collections::BTreeMap<String, RefusalRun>,
}

#[derive(Deserialize)]
struct RefusalRun {
    env: RefusalEnv,
    error: String,
}

#[derive(Deserialize)]
struct RefusalEnv {
    host: String,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    tls_ca: Option<String>,
}

#[test]
fn recorded_startup_refusals_reproduce_the_error_lines() {
    let path = testkit::recording("gateway/startup.json");
    let recording: Refusals =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("golden readable"))
            .expect("refusal golden parses");
    let tmp = tempdir().expect("temp root");
    let pki = tmp.path().join("pki");
    std::fs::create_dir_all(&pki).expect("pki dir");
    let cert = pki.join("node.crt");
    let key = pki.join("node.key");
    std::fs::write(&cert, b"cert").expect("cert fixture");
    std::fs::write(&key, b"key").expect("key fixture");
    for (name, run) in recording.runs {
        let tmp_text = tmp.path().to_str().expect("tmp");
        let material = |raw: &Option<String>| {
            raw.as_deref()
                .map(|path| testkit::substitute(path, &masks()))
                .map(|path| path.replacen("/tmp/golden", tmp_text, 1))
                .map(PathBuf::from)
        };
        let source = BindSource {
            host: run.env.host.clone(),
            port: 5174,
            tls_cert: material(&run.env.tls_cert),
            tls_key: material(&run.env.tls_key),
            tls_ca: material(&run.env.tls_ca),
            tls_crl: None,
            require_client: true,
            notices: Vec::new(),
        };
        let outcome = resolve_tls(&source);
        let expected = testkit::substitute(
            &testkit::translate_recorded_product_names(&run.error),
            &masks(),
        );
        let expected = expected.replacen("/tmp/golden", tmp_text, 1);
        match outcome {
            Err(error) => {
                assert_eq!(error.to_string(), expected, "run {name}");
            }
            Ok(_) => panic!("expected refusal for {name}"),
        }
    }
}

#[test]
fn tls_paths_resolve_when_the_files_exist() {
    let tmp = tempdir().expect("temp root");
    let cert = tmp.path().join("node.crt");
    let key = tmp.path().join("node.key");
    std::fs::write(&cert, b"cert").expect("cert fixture");
    std::fs::write(&key, b"key").expect("key fixture");
    let source = BindSource {
        host: "0.0.0.0".to_string(),
        port: 5174,
        tls_cert: Some(cert),
        tls_key: Some(key),
        tls_ca: None,
        tls_crl: None,
        require_client: true,
        notices: Vec::new(),
    };
    let paths = resolve_tls(&source)
        .expect("tls resolves")
        .expect("tls paths present");
    assert!(paths.ca.is_none());
}
#[test]
fn missing_cert_file_refuses_before_listen() {
    let source = BindSource {
        host: "0.0.0.0".to_string(),
        port: 5174,
        tls_cert: Some(std::path::PathBuf::from("/nonexistent/no-such-cert.pem")),
        tls_key: Some(std::path::PathBuf::from("/nonexistent/no-such-key.pem")),
        tls_ca: None,
        tls_crl: None,
        require_client: true,
        notices: Vec::new(),
    };
    let error = resolve_tls(&source).expect_err("missing cert refuses");
    assert!(matches!(error, BindError::Io(_)));
}

#[tokio::test]
async fn bind_loopback_hosts_and_the_recorded_refusal_hosts() {
    assert!(is_bind_loopback("127.0.0.1"));
    assert!(is_bind_loopback("localhost"));
    assert!(is_bind_loopback("::1"));
    assert!(is_bind_loopback("::ffff:127.0.0.1"));
    assert!(!is_bind_loopback("0.0.0.0"));
    assert!(!is_bind_loopback(""));
    assert!(!is_bind_loopback("gateway.example.test"));

    let claim = testkit::ports::claim().expect("claimed port");
    let port = claim.number();
    let source = BindSource {
        host: "localhost".to_string(),
        port,
        tls_cert: None,
        tls_key: None,
        tls_ca: None,
        tls_crl: None,
        require_client: true,
        notices: Vec::new(),
    };
    assert!(gateway::server::bind(&source).await.is_ok());
    let source = BindSource {
        host: "::1".to_string(),
        port,
        tls_cert: None,
        tls_key: None,
        tls_ca: None,
        tls_crl: None,
        require_client: true,
        notices: Vec::new(),
    };
    assert!(gateway::server::bind(&source).await.is_ok());

    let occupied = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("occupied listener");
    let port = occupied.local_addr().expect("addr").port();
    let source = BindSource {
        host: "127.0.0.1".to_string(),
        port,
        tls_cert: None,
        tls_key: None,
        tls_ca: None,
        tls_crl: None,
        require_client: true,
        notices: Vec::new(),
    };
    let error = gateway::server::bind(&source)
        .await
        .expect_err("port in use");
    assert!(matches!(error, BindError::Io(io) if io.kind() == std::io::ErrorKind::AddrInUse));
    drop(occupied);
}

#[test]
fn settings_come_from_the_config_gateway_section() {
    let document = "gateway:\n  host: 0.0.0.0\n  port: 6001\n  allowed_origins:\n    - https://hub.example.test\n  allowed_hosts:\n    - gateway.example.test\n";
    let config: config::Config = serde_yaml_ng::from_str(document).expect("config");
    let home = tempdir().expect("home");
    let env = config::Environment::from_vars([("HOME", home.path().to_str().expect("utf-8 home"))]);
    let bind = BindSource::resolve(&config, &env, "golden-node", home.path());
    assert_eq!(bind.host, "0.0.0.0");
    assert_eq!(bind.port, 6001);
    let settings = gateway::Settings::from_config_in(
        &config,
        &env,
        "gold-host".to_string(),
        "golden-node".to_string(),
    )
    .expect("settings");
    assert_eq!(settings.allowed_origins, vec!["https://hub.example.test"]);
    assert_eq!(settings.allowed_hosts, vec!["gateway.example.test"]);

    let empty: config::Config = serde_yaml_ng::from_str("").expect("empty config");
    let bind = BindSource::resolve(&empty, &env, "golden-node", home.path());
    assert_eq!(bind.host, "127.0.0.1");
    assert_eq!(bind.port, 5174);
}

#[test]
fn surface_paths_follow_the_environment_then_the_gateway_section() {
    let document = "gateway:\n  static_dir: /yaml/static\n  root_redirect: /yaml\n  devices:\n    enabled: true\n";
    let config: config::Config = serde_yaml_ng::from_str(document).expect("config");
    let legacy = config::Environment::from_vars([
        ("HOME", "/tmp/golden/home"),
        ("RIVETOS_SHARED_DIR", "/tmp/golden/shared"),
        ("RIVETOS_DEN_MESH_FILE", "/tmp/golden/mesh.json"),
        ("RIVETOS_DEN_STATE_DIR", "/tmp/golden/state"),
        ("RIVETOS_DEN_STATIC_DIR", "/tmp/golden/static"),
        ("RIVETOS_DEN_ROOT_REDIRECT", "/wiki"),
        ("RIVETOS_DEN_MESH_CACHE_MS", "0"),
    ]);
    let settings = gateway::Settings::from_config_in(
        &config,
        &legacy,
        "gold-host".to_string(),
        "golden-node".to_string(),
    )
    .expect("legacy settings");
    assert_eq!(
        settings.static_dir.as_deref(),
        Some(std::path::Path::new("/tmp/golden/static"))
    );
    assert_eq!(settings.root_redirect.as_deref(), Some("/wiki"));
    assert_eq!(settings.state_dir, PathBuf::from("/tmp/golden/state"));
    assert_eq!(
        settings.mesh_file.as_deref(),
        Some(std::path::Path::new("/tmp/golden/mesh.json"))
    );
    assert_eq!(settings.shared_dir, PathBuf::from("/tmp/golden/shared"));
    assert_eq!(
        settings.legacy_mesh_dir,
        PathBuf::from("/tmp/golden/home/.rivetos")
    );
    assert!(settings.devices_enabled);
    assert_eq!(settings.mesh_cache_ttl, std::time::Duration::ZERO);
    assert_eq!(
        settings.probe_timeout,
        std::time::Duration::from_millis(1_500)
    );
}

#[test]
fn mesh_cache_duration_rejects_a_bad_count_and_defaults_when_unset() {
    let document = "gateway:\n  static_dir: /yaml/static\n  root_redirect: /yaml\n  devices:\n    enabled: true\n";
    let config: config::Config = serde_yaml_ng::from_str(document).expect("config");
    let current = config::Environment::from_vars([
        ("HOME", "/tmp/golden/home"),
        ("RIVETHUB_MESH_FILE", "/tmp/golden/current.json"),
        ("RIVETOS_DEN_MESH_FILE", "/tmp/golden/legacy.json"),
        ("RIVETHUB_MESH_CACHE_MS", "0"),
    ]);
    let settings = gateway::Settings::from_config_in(
        &config,
        &current,
        "gold-host".to_string(),
        "golden-node".to_string(),
    )
    .expect("current settings");
    assert_eq!(
        settings.mesh_file.as_deref(),
        Some(std::path::Path::new("/tmp/golden/current.json"))
    );
    assert_eq!(settings.mesh_cache_ttl, std::time::Duration::ZERO);

    let bad_cache = config::Environment::from_vars([
        ("HOME", "/tmp/golden/home"),
        ("RIVETHUB_MESH_CACHE_MS", "nope"),
    ]);
    let error = gateway::Settings::from_config_in(
        &config,
        &bad_cache,
        "gold-host".to_string(),
        "golden-node".to_string(),
    )
    .expect_err("bad cache duration");
    assert_eq!(
        error.to_string(),
        "RIVETHUB_MESH_CACHE_MS=nope is not a millisecond count"
    );

    let bare = config::Environment::from_vars([("HOME", "/tmp/golden/home")]);
    let settings = gateway::Settings::from_config_in(
        &config,
        &bare,
        "gold-host".to_string(),
        "golden-node".to_string(),
    )
    .expect("bare settings");
    assert_eq!(settings.mesh_cache_ttl, std::time::Duration::from_secs(10));
    assert_eq!(
        settings.static_dir.as_deref(),
        Some(std::path::Path::new("/yaml/static"))
    );
    assert_eq!(settings.root_redirect.as_deref(), Some("/yaml"));
    assert_eq!(
        settings.state_dir,
        PathBuf::from("/tmp/golden/home/.rivethub")
    );
    assert!(settings.mesh_file.is_none());
}

#[test]
fn bind_error_lines_carry_the_recorded_text() {
    let error = BindError::TlsRequired {
        host: "0.0.0.0".to_string(),
    };
    assert_eq!(
        error.to_string(),
        "[rivethub-gateway] refusing to bind 0.0.0.0: gateway TLS is required off-loopback (set RIVETHUB_TLS_CERT + RIVETHUB_TLS_KEY to the node's holder certificate, ~/.rivethub/tls/holder.cert.pem and holder.key.pem from rh mesh enroll; devices enroll with rh cert renew). Or bind the gateway host to 127.0.0.1."
    );
    let error = BindError::CaMissing {
        path: std::path::PathBuf::from("/tmp/golden/chain.pem"),
    };
    assert_eq!(
        error.to_string(),
        "[rivethub-gateway] TLS CA chain missing at /tmp/golden/chain.pem — cannot verify device client certs"
    );
}

#[test]
fn mesh_peers_are_probed_against_the_shared_rivet_ca_when_the_gateway_names_none() {
    let shared = tempdir().expect("shared dir");
    let chain = shared.path().join("rivet-ca").join("intermediate");
    std::fs::create_dir_all(&chain).expect("ca dir");
    std::fs::write(chain.join("chain.pem"), b"pem").expect("chain");
    let config: config::Config =
        serde_yaml_ng::from_str("gateway:\n  port: 5174\n").expect("config");
    let shared_text = shared.path().to_str().expect("utf-8 path");
    let settings = |vars: Vec<(&str, &str)>| {
        gateway::Settings::from_config_in(
            &config,
            &config::Environment::from_vars(vars),
            "gold-host".to_string(),
            "golden-node".to_string(),
        )
        .expect("settings")
    };

    let fallback = settings(vec![
        ("HOME", "/tmp/golden/home"),
        ("RIVETHUB_SHARED_DIR", shared_text),
    ]);
    assert_eq!(
        fallback.mesh_ca.as_deref(),
        Some(
            fallback
                .shared_dir
                .join("rivet-ca/intermediate/chain.pem")
                .as_path()
        )
    );

    let named = shared.path().join("named.pem");
    std::fs::write(&named, b"pem").expect("named chain");
    let named_text = named.to_str().expect("utf-8 path");
    let chosen = settings(vec![
        ("HOME", "/tmp/golden/home"),
        ("RIVETHUB_SHARED_DIR", shared_text),
        ("RIVET_CA_CHAIN", named_text),
    ]);
    assert_eq!(chosen.mesh_ca.as_deref(), Some(named.as_path()));

    let missing = settings(vec![
        ("HOME", "/tmp/golden/home"),
        ("RIVETHUB_SHARED_DIR", "/tmp/golden/no-such-shared"),
    ]);
    assert_eq!(missing.mesh_ca, None);
}
