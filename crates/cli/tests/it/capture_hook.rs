use crate::support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cli::Exit;
use gateway::server::BindSource;
use gateway::tenancy::{Tenancy, UserId};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use support::{config, environment, home, run};
use testkit::{FakeMemoryStore, MemoryRequest};
use tokio_util::sync::CancellationToken;

const PROMPT: &str = r#"{"session_id":"55555555-5555-5555-5555-555555555555","cwd":"/work/demo","hook_event_name":"UserPromptSubmit","prompt":"list the files"}"#;
const TOOL: &str = r#"{"session_id":"55555555-5555-5555-5555-555555555555","cwd":"/work/demo","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"toolu_1"}"#;

fn now() -> Result<chrono::DateTime<chrono::Utc>, chrono::ParseError> {
    "2026-10-10T12:00:00Z".parse()
}

#[tokio::test]
async fn the_hook_never_fails_the_harness_without_a_config()
-> Result<(), Box<dyn std::error::Error>> {
    let home = home();
    let env = environment(&home);
    let report = cli::capture_hook::hook(&env, PROMPT, now()?).await;
    assert_eq!(report.exit, Exit::Ok);
    assert_eq!(report.stdout, "");
    assert!(
        report
            .stderr
            .starts_with("capture hook is not configured: "),
        "{}",
        report.stderr
    );
    Ok(())
}

#[tokio::test]
async fn an_unreachable_gateway_spools_the_batch() -> Result<(), Box<dyn std::error::Error>> {
    let port = testkit::ports::claim()?;
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    let report = cli::capture_hook::hook(&env, TOOL, now()?).await;
    assert_eq!(report.exit, Exit::Ok);
    assert!(
        report.stderr.starts_with("capture request failed"),
        "{}",
        report.stderr
    );
    let spooled = std::fs::read_dir(home.path().join(".rivethub/capture-spool"))?.count();
    assert_eq!(spooled, 1);
    Ok(())
}

#[tokio::test]
async fn the_capture_command_lists_the_hook_subcommand() {
    let home = home();
    let env = environment(&home);
    let report = run(&["capture", "--help"], &env).await;
    assert_eq!(report.exit, Exit::Ok);
    assert!(
        report
            .stdout
            .contains("hook  Capture one harness hook payload read from stdin"),
        "{}",
        report.stdout
    );
}

#[tokio::test]
async fn a_device_identity_captures_through_a_remote_gateway_as_its_mapped_user()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let home = home();
    let env = environment(&home);
    let tls = home.path().join("tls");
    std::fs::create_dir_all(&tls)?;
    let root_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    let mut root = CertificateParams::new(Vec::<String>::new())?;
    root.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    root.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    std::fs::write(tls.join("ca.pem"), root.self_signed(&root_key)?.pem())?;
    let issuer = Issuer::from_params(&root, &root_key);
    let mut host = CertificateParams::new(vec!["127.0.0.1".to_owned()])?;
    host.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf(&tls, "server", &host, &issuer)?;
    let mut device = CertificateParams::default();
    device.subject_alt_names = vec![SanType::URI(
        "spiffe://rivet.mesh/device/member/member-laptop".try_into()?,
    )];
    device.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    leaf(&tls, "device", &device, &issuer)?;

    let store = Arc::new(FakeMemoryStore::default());
    let tenancy = Tenancy::local(UserId::new("owner".to_owned())?)
        .with_device("member-laptop", UserId::new("member".to_owned())?)?;
    let gateway =
        gateway::Gateway::new(Arc::clone(&store) as _, settings(home.path())).with_tenancy(tenancy);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let tls_paths = gateway::tls::resolve_tls(&BindSource {
        host: "127.0.0.1".to_owned(),
        port,
        tls_cert: Some(tls.join("server.pem")),
        tls_key: Some(tls.join("server.key")),
        tls_ca: Some(tls.join("ca.pem")),
        tls_crl: None,
        require_client: true,
        notices: Vec::new(),
    })?;
    let stop = CancellationToken::new();
    let server = tokio::spawn(gateway::server::serve(
        listener,
        Arc::new(gateway),
        tls_paths,
        stop.clone(),
    ));

    let own = testkit::ports::claim()?;
    config(
        &home,
        &format!(
            "gateway:\n  host: 127.0.0.1\n  port: {own}\nmemory:\n  capture:\n    gateway:\n      url: https://127.0.0.1:{port}\n      ca: {}\n      cert: {}\n      key: {}\n",
            tls.join("ca.pem").display(),
            tls.join("device.pem").display(),
            tls.join("device.key").display(),
        ),
    );
    let report = cli::capture_hook::hook(&env, TOOL, now()?).await;
    let owners: Vec<String> = store
        .requests()
        .await
        .into_iter()
        .filter_map(|request| {
            if let MemoryRequest::Append(owner, _) = request {
                Some(owner.as_str().to_owned())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(owners, ["member"], "{}", report.stderr);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await???;
    Ok(())
}

fn leaf(
    directory: &Path,
    name: &str,
    params: &CertificateParams,
    issuer: &Issuer<'_, &KeyPair>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    std::fs::write(
        directory.join(format!("{name}.pem")),
        params.signed_by(&key, issuer)?.pem(),
    )?;
    std::fs::write(directory.join(format!("{name}.key")), key.serialize_pem())?;
    Ok(())
}

fn settings(home: &Path) -> gateway::Settings {
    gateway::Settings {
        hostname: "gold-host".to_owned(),
        node: "golden-node".to_owned(),
        allowed_origins: Vec::new(),
        allowed_hosts: Vec::new(),
        files_root: None,
        static_dir: None,
        root_redirect: None,
        state_dir: home.join("gateway"),
        directory_root: home.join("agents"),
        mesh_file: None,
        shared_dir: home.join("shared"),
        legacy_mesh_dir: home.join("legacy"),
        allowed_harnesses: None,
        devices_enabled: false,
        probe_timeout: Duration::from_millis(200),
        mesh_cache_ttl: Duration::ZERO,
        mesh_ca: None,
    }
}
