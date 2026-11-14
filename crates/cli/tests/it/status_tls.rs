use crate::support;

use std::sync::Arc;

use axum::{Json, Router, http::HeaderMap, routing::get};
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
    hosts: Arc<parking_lot::Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

async fn tls_health(dns_name: &str) -> Result<Served> {
    tls_health_named(&[dns_name]).await
}

async fn tls_health_named(dns_names: &[&str]) -> Result<Served> {
    let ca_key = KeyPair::generate()?;
    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = ca_params.self_signed(&ca_key)?;
    let issuer = Issuer::from_params(&ca_params, &ca_key);
    let node_key = KeyPair::generate()?;
    let mut node_params = CertificateParams::new(
        dns_names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
    )?;
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
    let hosts = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen = Arc::clone(&hosts);
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(stream).await else {
                    return;
                };
                let router = Router::new().route(
                    "/healthz",
                    get(move |headers: HeaderMap| async move {
                        if let Some(host) =
                            headers.get("host").and_then(|value| value.to_str().ok())
                        {
                            seen.lock().push(host.to_owned());
                        }
                        Json(HealthzResponse {
                            ok: true,
                            sessions: 0,
                            name: "host".to_owned(),
                            node: "node".to_owned(),
                            degraded: None,
                            degraded_providers: Vec::new(),
                            version: None,
                            revision: None,
                            policy: None,
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
        hosts,
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

#[tokio::test]
async fn status_keeps_a_loopback_name_when_the_certificate_carries_one() -> Result {
    let served = tls_health_named(&["node.mesh.test", "localhost"]).await?;
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
    assert_eq!(status.stdout, "RivetHub is running\n", "{}", status.stderr);
    let hosts = served.hosts.lock().clone();
    assert!(
        !hosts.is_empty() && hosts.iter().all(|host| host.starts_with("localhost")),
        "the owner gate only trusts a loopback Host, so the client must not swap in the mesh name: {hosts:?}"
    );
    served.task.abort();
    Ok(())
}

#[tokio::test]
async fn status_trusts_the_root_the_config_names_without_any_variable() -> Result {
    let served = tls_health("node.mesh.test").await?;
    let home = home();
    let dir = home.path().join("tls");
    let env = ::config::Environment::from_vars([("HOME".as_ref(), home.path().as_os_str())]);
    std::fs::create_dir_all(&dir)?;
    for (name, body) in [
        ("ca.pem", &served.ca_pem),
        ("node.pem", &served.cert_pem),
        ("node.key", &served.key_pem),
    ] {
        std::fs::write(dir.join(name), body)?;
    }
    let gateway = |trust: &str| {
        format!(
            "gateway:\n  host: 0.0.0.0\n  port: {}\n  tls_cert: {}\n  tls_key: {}\n{trust}",
            served.port,
            dir.join("node.pem").display(),
            dir.join("node.key").display(),
        )
    };
    config(&home, &gateway(""));
    let untrusted = run(&["status"], &env).await;
    assert!(
        untrusted.stderr.contains("Health unavailable"),
        "without a trust root the private CA is unknown: {}",
        untrusted.stderr
    );
    config(
        &home,
        &gateway(&format!("  trust_root: {}\n", dir.join("ca.pem").display())),
    );
    let status = run(&["status"], &env).await;
    assert_eq!(status.exit, Exit::Ok);
    assert_eq!(status.stdout, "RivetHub is running\n", "{}", status.stderr);
    served.task.abort();
    Ok(())
}
