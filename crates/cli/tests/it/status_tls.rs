use crate::support;

use std::sync::Arc;

use axum::{Json, Router, routing::get};
use cli::Exit;
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use support::{config, home, run};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use wire::gateway::HealthzResponse;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Served {
    port: u16,
    ca_pem: String,
    cert_pem: String,
    key_pem: String,
    task: tokio::task::JoinHandle<()>,
}

async fn tls_health(dns_name: &str) -> Result<Served> {
    let ca_key = KeyPair::generate()?;
    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = ca_params.self_signed(&ca_key)?;
    let issuer = Issuer::from_params(&ca_params, &ca_key);
    let node_key = KeyPair::generate()?;
    let mut node_params = CertificateParams::new(vec![dns_name.to_owned()])?;
    node_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let node = node_params.signed_by(&node_key, &issuer)?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let server = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(
            vec![node.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(node_key.serialize_der()).into(),
        )?;
    let acceptor = TlsAcceptor::from(Arc::new(server));
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(stream).await else {
                    return;
                };
                let router = Router::new().route(
                    "/healthz",
                    get(|| async {
                        Json(HealthzResponse {
                            ok: true,
                            sessions: 0,
                            name: "host".to_owned(),
                            node: "node".to_owned(),
                            degraded: None,
                            degraded_providers: Vec::new(),
                            version: None,
                            revision: None,
                        })
                    }),
                );
                drop(
                    hyper::server::conn::http1::Builder::new()
                        .serve_connection(
                            hyper_util::rt::TokioIo::new(tls),
                            hyper_util::service::TowerToHyperService::new(router),
                        )
                        .await,
                );
            });
        }
    });
    Ok(Served {
        port,
        ca_pem: ca.pem(),
        cert_pem: node.pem(),
        key_pem: node_key.serialize_pem(),
        task,
    })
}

#[tokio::test]
async fn status_verifies_a_tls_gateway_by_the_name_its_certificate_carries() -> Result {
    let served = tls_health("node.mesh.test").await?;
    let home = home();
    let dir = home.path().join("tls");
    let ca = dir.join("ca.pem");
    let env = ::config::Environment::from_vars([
        ("HOME".as_ref(), home.path().as_os_str()),
        ("RIVETHUB_TLS_CA".as_ref(), ca.as_os_str()),
    ]);
    std::fs::create_dir_all(&dir)?;
    for (name, body) in [
        ("ca.pem", &served.ca_pem),
        ("node.pem", &served.cert_pem),
        ("node.key", &served.key_pem),
    ] {
        std::fs::write(dir.join(name), body)?;
    }
    config(
        &home,
        &format!(
            "gateway:\n  host: 0.0.0.0\n  port: {}\n  tls_cert: {}\n  tls_key: {}\n",
            served.port,
            dir.join("node.pem").display(),
            dir.join("node.key").display(),
        ),
    );
    let status = run(&["status"], &env).await;
    assert_eq!(status.exit, Exit::Ok);
    assert_eq!(status.stdout, "RivetHub is running\n", "{}", status.stderr);
    assert!(
        !status.stderr.contains("Health unavailable"),
        "{}",
        status.stderr
    );

    let stranger = tls_health("node.mesh.test").await?;
    std::fs::write(dir.join("ca.pem"), &stranger.ca_pem)?;
    let refused = run(&["status"], &env).await;
    assert!(
        refused.stderr.contains("Health unavailable"),
        "{}",
        refused.stderr
    );
    served.task.abort();
    stranger.task.abort();
    Ok(())
}
