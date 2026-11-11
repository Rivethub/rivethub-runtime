use std::sync::Arc;
use std::time::Duration;

use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[tokio::test]
async fn composed_gateway_serves_https_from_the_shared_issued_pair() -> TestResult {
    let home = tempfile::tempdir()?;
    let shared = tempfile::tempdir()?;
    let ca = write_issued(shared.path(), "fallback-node")?;
    let env = config::Environment::from_vars([
        ("HOME", home.path().to_str().ok_or("utf-8 home")?),
        (
            "RIVETHUB_SHARED_DIR",
            shared.path().to_str().ok_or("utf-8 shared")?,
        ),
        ("RIVETHUB_NODE_NAME", "fallback-node"),
    ]);
    let config: config::Config = serde_yaml_ng::from_str("gateway:\n  host: 0.0.0.0\n  port: 0\n")?;
    let store: Arc<dyn kernel::memory::MemoryStore> = Arc::new(testkit::FakeMemoryStore::default());
    let shutdown = CancellationToken::new();
    let composition = rivethub::composition::Composition::build(
        &config,
        &env,
        store,
        &shutdown,
        &kernel::shutdown::Activities::new(),
    )
    .await?;
    let (listener, tls) = rivethub::service::listen(&config, &env).await?;
    let paths = tls.as_ref().ok_or("shared fallback did not enable tls")?;
    assert!(paths.cert.ends_with("fallback-node.crt"));
    assert!(paths.key.ends_with("fallback-node.key"));
    let port = listener.local_addr()?.port();
    let gateway = Arc::clone(composition.gateway());
    let server_shutdown = shutdown.clone();
    let server = tokio::spawn(async move {
        rivethub::service::serve(listener, tls, gateway, server_shutdown).await
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(ca.as_bytes())?)
        .timeout(Duration::from_secs(5))
        .build()?;
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        client
            .get(format!("https://127.0.0.1:{port}/healthz"))
            .send(),
    )
    .await??;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    shutdown.cancel();
    server.await??;
    composition.close().await?;
    Ok(())
}

fn write_issued(shared: &std::path::Path, node: &str) -> TestResult<String> {
    let root_key = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec!["rivet-ca.example.test".into()])?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let root = params.self_signed(&root_key)?;
    let issuer = rcgen::Issuer::from_params(&params, &root_key);
    let node_key = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec!["127.0.0.1".into(), "localhost".into()])?;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    let certificate = params.signed_by(&node_key, &issuer)?;
    let issued_dir = shared.join("rivet-ca/issued");
    std::fs::create_dir_all(&issued_dir)?;
    std::fs::create_dir_all(shared.join("rivet-ca/intermediate"))?;
    std::fs::write(issued_dir.join(format!("{node}.crt")), certificate.pem())?;
    std::fs::write(
        issued_dir.join(format!("{node}.key")),
        node_key.serialize_pem(),
    )?;
    std::fs::write(shared.join("rivet-ca/intermediate/chain.pem"), root.pem())?;
    Ok(root.pem())
}
