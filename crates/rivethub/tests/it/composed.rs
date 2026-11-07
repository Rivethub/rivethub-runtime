use crate::process;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use process::{Running, TestResult};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use sqlx::ConnectOptions;

struct Certificates {
    root: rcgen::Certificate,
    peer: rcgen::Certificate,
    peer_key: KeyPair,
    unknown: reqwest::Identity,
    legacy: reqwest::Identity,
    revoked: reqwest::Identity,
}

fn certificates(directory: &std::path::Path) -> TestResult<Certificates> {
    let root_key = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec!["root.example.test".into()])?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::CrlSign,
    ];
    let root = params.self_signed(&root_key)?;
    let issuer = rcgen::Issuer::from_params(&params, &root_key);
    let node_key = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec!["127.0.0.1".into(), "localhost".into()])?;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let node = params.signed_by(&node_key, &issuer)?;
    std::fs::write(directory.join("node.pem"), node.pem())?;
    std::fs::write(directory.join("node.key"), node_key.serialize_pem())?;
    std::fs::write(directory.join("ca.pem"), root.pem())?;
    let peer_key = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec!["device.example.test".into()])?;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.subject_alt_names.push(rcgen::SanType::URI(
        "spiffe://rivet.mesh/device/device-user/test".try_into()?,
    ));
    let peer = params.signed_by(&peer_key, &issuer)?;
    let legacy_key = KeyPair::generate()?;
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "device:test");
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let legacy = params.signed_by(&legacy_key, &issuer)?;
    let legacy = reqwest::Identity::from_pem(
        format!("{}{}", legacy.pem(), legacy_key.serialize_pem()).as_bytes(),
    )?;
    let unknown_key = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec!["unknown.example.test".into()])?;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.subject_alt_names.push(rcgen::SanType::URI(
        "spiffe://rivet.mesh/device/device-user/unknown".try_into()?,
    ));
    let unknown = params.signed_by(&unknown_key, &issuer)?;
    let unknown = reqwest::Identity::from_pem(
        format!("{}{}", unknown.pem(), unknown_key.serialize_pem()).as_bytes(),
    )?;
    let revoked_key = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec!["revoked.example.test".into()])?;
    params.serial_number = Some(42.into());
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let revoked = params.signed_by(&revoked_key, &issuer)?;
    let revoked = reqwest::Identity::from_pem(
        format!("{}{}", revoked.pem(), revoked_key.serialize_pem()).as_bytes(),
    )?;
    let crl = rcgen::CertificateRevocationListParams {
        this_update: rcgen::date_time_ymd(2025, 1, 1),
        next_update: rcgen::date_time_ymd(2035, 1, 1),
        crl_number: 1.into(),
        issuing_distribution_point: None,
        revoked_certs: vec![rcgen::RevokedCertParams {
            serial_number: 42.into(),
            revocation_time: rcgen::date_time_ymd(2025, 1, 1),
            reason_code: None,
            invalidity_date: None,
        }],
        key_identifier_method: rcgen::KeyIdMethod::Sha256,
    }
    .signed_by(&issuer)?;
    std::fs::write(directory.join("revoked.pem"), crl.pem()?)?;
    write_registry(directory)?;
    Ok(Certificates {
        root,
        peer,
        peer_key,
        unknown,
        legacy,
        revoked,
    })
}

fn write_registry(directory: &std::path::Path) -> TestResult {
    let users = wire::gateway::users::UsersRegistry {
        owner_user_id: "owner".into(),
        unmapped_is_owner: false,
        local_stores: None,
        spaces: None,
        users: BTreeMap::from([(
            "device-user".into(),
            wire::gateway::users::UserRecord {
                id: None,
                devices: Some(vec!["test".into()]),
                pg_url: None,
                env_file: None,
                persona: None,
                admin: None,
            },
        )]),
    };
    std::fs::write(directory.join("users.json"), serde_json::to_vec(&users)?)?;
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn composed_binary_serves_tls_memory_and_websocket(pool: sqlx::PgPool) -> TestResult {
    migrate::postgres_pool(&pool).await?;
    let url = pool.connect_options().to_url_lossy();
    let home = tempfile::tempdir()?;
    let state = home.path().join(".rivethub");
    std::fs::create_dir(&state)?;
    let material = certificates(&state)?;
    let provider = process::provider::Provider::start().await?;
    let model_address = provider.address;
    let port = testkit::ports::claim()?;
    let config = format!(
        "runtime:\n  workspace: {}\n  default_agent: main\nagents:\n  main:\n    provider: p\n    model: test\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://{model_address}\n    model: test\nmemory:\n  backend: postgres\n  postgres:\n    connection_string: {url}\ngateway:\n  host: 127.0.0.1\n  port: {port}\n  tls_cert: {}/node.pem\n  tls_key: {}/node.key\n  tls_ca: {}/ca.pem\n  users_file: {}/users.json\n  tls_crl: {}/revoked.pem\nmesh:\n  node_name: test-node\n",
        home.path().display(),
        state.display(),
        state.display(),
        state.display(),
        state.display(),
        state.display()
    );
    std::fs::write(state.join("config.yaml"), config)?;
    let running = Running::start(home.path()).await?;
    let address = running.address;
    let identity = reqwest::Identity::from_pem(
        format!(
            "{}{}",
            material.peer.pem(),
            material.peer_key.serialize_pem()
        )
        .as_bytes(),
    )?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(
            material.root.pem().as_bytes(),
        )?)
        .identity(identity)
        .timeout(Duration::from_secs(10))
        .build()?;
    let health: wire::gateway::HealthzResponse = client
        .get(format!("https://{address}/healthz"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(health.ok);
    assert_eq!(health.sessions, 0);
    let stats: wire::gateway::memory::MemoryStatsResponse = client
        .get(format!("https://{address}/api/memory/stats"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(stats.messages, 0);
    let legacy = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(
            material.root.pem().as_bytes(),
        )?)
        .identity(material.legacy)
        .timeout(Duration::from_secs(10))
        .build()?;
    let legacy_stats: wire::gateway::memory::MemoryStatsResponse = legacy
        .get(format!("https://{address}/api/memory/stats"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(legacy_stats.messages, 0);
    drop(legacy);
    let revoked = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(
            material.root.pem().as_bytes(),
        )?)
        .identity(material.revoked)
        .timeout(Duration::from_secs(10))
        .build()?;
    assert!(
        revoked
            .get(format!("https://{address}/healthz"))
            .send()
            .await
            .is_err()
    );
    let unknown = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(
            material.root.pem().as_bytes(),
        )?)
        .identity(material.unknown)
        .timeout(Duration::from_secs(10))
        .build()?;
    assert_eq!(
        unknown
            .get(format!("https://{address}/api/memory/stats"))
            .send()
            .await?
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let mut roots = rustls::RootCertStore::empty();
    roots.add(material.root.der().clone())?;
    let client_tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_client_auth_cert(
        vec![material.peer.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(material.peer_key.serialize_der()).into(),
    )?;
    let (mut websocket, _) = tokio_tungstenite::connect_async_tls_with_config(
        format!("wss://{address}/ws"),
        None,
        false,
        Some(tokio_tungstenite::Connector::Rustls(Arc::new(client_tls))),
    )
    .await?;
    let frame = tokio::time::timeout(Duration::from_secs(5), websocket.next())
        .await?
        .ok_or("no snapshot")??;
    let snapshot: wire::events::Snapshot = serde_json::from_str(frame.to_text()?)?;
    assert_eq!(
        snapshot,
        wire::events::Snapshot {
            v: wire::events::ProtocolVersion::V1,
            sessions: Vec::new(),
            views: BTreeMap::new()
        }
    );
    websocket.close(None).await?;
    drop(websocket);
    drop(client);
    drop(unknown);
    let lines = running.stop(nix::sys::signal::Signal::SIGINT).await?;
    assert!(
        lines
            .iter()
            .any(|line| line.contains("SIGINT received — shutting down"))
    );
    assert!(lines.iter().any(|line| line.contains("gateway stopped")));
    provider.close().await?;
    Ok(())
}

#[tokio::test]
async fn check_validates_without_moving_legacy_state_or_connecting_postgres() -> TestResult {
    let home = tempfile::tempdir()?;
    let legacy = home.path().join(".rivetos");
    std::fs::create_dir(&legacy)?;
    std::fs::write(
        legacy.join("config.yaml"),
        format!(
            "runtime:\n  workspace: {}\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    model: m\n",
            home.path().display()
        ),
    )?;
    std::fs::write(legacy.join("rivetos.pid"), std::process::id().to_string())?;
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_rh"))
            .args(["gateway", "--check"])
            .env_clear()
            .env("HOME", home.path())
            .env("TOKIO_WORKER_THREADS", "2")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|error| format!("rh gateway --check exceeded the command deadline: {error}"))??;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(legacy.is_dir());
    assert!(!home.path().join(".rivethub").exists());
    let refusal = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_rh"))
            .arg("gateway")
            .env_clear()
            .env("HOME", home.path())
            .env("TOKIO_WORKER_THREADS", "2")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|error| format!("rh gateway exceeded the command deadline: {error}"))??;
    assert_eq!(refusal.status.code(), Some(1));
    assert!(
        String::from_utf8(refusal.stderr)?.contains("refusing to move legacy state: live process")
    );
    assert!(legacy.is_dir());
    assert!(!home.path().join(".rivethub").exists());
    Ok(())
}
