use config::model::{GatewaySection, MemorySection, PostgresSection};
use config::{Config, Environment};

fn isolated(dir: &std::path::Path) -> Environment {
    Environment::from_vars([
        ("HOME", dir.to_string_lossy().as_ref()),
        ("RIVETHUB_SHARED_DIR", dir.to_string_lossy().as_ref()),
        ("HOSTNAME", "recall-test"),
    ])
}

fn gateway(host: Option<&str>, port: u16, enabled: Option<bool>) -> GatewaySection {
    GatewaySection {
        host: host.map(str::to_owned),
        port,
        enabled,
        ..GatewaySection::default()
    }
}

fn postgres(url: &str) -> MemorySection {
    MemorySection {
        postgres: Some(PostgresSection {
            connection_string: Some(url.to_owned()),
            ..PostgresSection::default()
        }),
        ..MemorySection::default()
    }
}

fn node(memory: Option<MemorySection>, gateway: Option<GatewaySection>) -> Config {
    Config {
        memory,
        gateway,
        ..Config::default()
    }
}

async fn origin(config: &Config, env: &Environment) -> Option<String> {
    cli::recall_origin(env, config)
        .await
        .as_ref()
        .map(ToString::to_string)
}

#[tokio::test]
async fn a_local_node_uses_the_gateway_rh_open_would_print() {
    let home = tempfile::tempdir().expect("home");
    let env = isolated(home.path());
    let local = origin(&Config::default(), &env).await;
    assert_eq!(local.as_deref(), Some("http://127.0.0.1:5174/"));
    let custom = node(None, Some(gateway(Some("0.0.0.0"), 5999, None)));
    assert_eq!(
        origin(&custom, &env).await.as_deref(),
        Some("http://127.0.0.1:5999/")
    );
    let loopback = node(
        Some(postgres("postgres://rivet@127.0.0.1/memory")),
        Some(gateway(None, 5174, None)),
    );
    assert_eq!(
        origin(&loopback, &env).await.as_deref(),
        Some("http://127.0.0.1:5174/")
    );
    assert_eq!(
        origin(
            &node(Some(postgres("postgres://rivet@localhost/memory")), None),
            &env
        )
        .await
        .as_deref(),
        Some("http://127.0.0.1:5174/")
    );
    assert_eq!(
        origin(&node(Some(postgres("postgres:///memory")), None), &env)
            .await
            .as_deref(),
        Some("http://127.0.0.1:5174/")
    );
}

#[tokio::test]
async fn an_unknown_origin_adds_no_link_target() {
    let home = tempfile::tempdir().expect("home");
    let env = isolated(home.path());
    let disabled = node(None, Some(gateway(Some("127.0.0.1"), 5174, Some(false))));
    assert_eq!(origin(&disabled, &env).await, None);
    assert_eq!(
        origin(
            &node(Some(postgres("postgres://user@db.example/memory")), None),
            &env
        )
        .await,
        None
    );
    assert_eq!(
        origin(
            &node(Some(postgres("host=db.example.com dbname=memory")), None),
            &env
        )
        .await,
        None
    );
    let declared = node(
        Some(postgres("postgres://user@db.example/memory")),
        Some(gateway(Some("203.0.113.10"), 5174, None)),
    );
    assert_eq!(
        origin(&declared, &env).await.as_deref(),
        Some("http://203.0.113.10:5174/")
    );
}

#[tokio::test]
async fn a_loopback_certificate_named_localhost_is_the_open_origin() {
    let home = tempfile::tempdir().expect("home");
    let key = rcgen::KeyPair::generate().expect("key");
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).expect("params");
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let cert = params.self_signed(&key).expect("cert");
    let dir = home.path();
    let cert_path = dir.join("node.pem");
    let ca_path = dir.join("ca.pem");
    std::fs::write(&cert_path, cert.pem()).expect("cert");
    std::fs::write(&ca_path, cert.pem()).expect("ca");
    let config = node(
        None,
        Some(GatewaySection {
            host: Some("127.0.0.1".to_owned()),
            port: 5174,
            tls_cert: Some(cert_path.display().to_string()),
            tls_key: Some(dir.join("node.key").display().to_string()),
            trust_root: Some(ca_path),
            ..GatewaySection::default()
        }),
    );
    assert_eq!(
        origin(&config, &isolated(dir)).await.as_deref(),
        Some("https://localhost:5174/")
    );
}
