use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gateway::server::ServeOptions;
use gateway::tls::TlsPaths;
mod issuer;

use kernel::ca::CertificateAuthority;
use rcgen::{
    CertificateParams, ExtendedKeyUsagePurpose, Issuer, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::CertificateRevocationListDer;
use rustls::pki_types::pem::PemObject;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use wire::ca::SignRequest;
use x509_parser::prelude::FromDer;

pub(crate) type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub(crate) struct Material {
    directory: tempfile::TempDir,
    pub(crate) pki: Arc<issuer::TestAuthority>,
    state: PathBuf,
    pub(crate) intermediate: String,
    intermediate_key: String,
    root_pem: String,
    crl: Option<PathBuf>,
}

pub(crate) struct Lab {
    _directory: tempfile::TempDir,
    pub(crate) pki: Arc<issuer::TestAuthority>,
    pub(crate) intermediate: String,
    root: reqwest::Certificate,
    base: String,
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<std::result::Result<(), gateway::server::ServeError>>>,
}

pub(crate) struct Reply {
    pub(crate) status: reqwest::StatusCode,
    pub(crate) content_type: Option<String>,
    pub(crate) body: String,
}

impl Material {
    pub(crate) fn open() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let state = directory.path().join("state");
        std::fs::create_dir(&state)?;
        let generated = issuer::generate()?;
        Ok(Self {
            directory,
            pki: Arc::new(generated.issuer),
            state,
            intermediate: generated.intermediate,
            intermediate_key: generated.intermediate_key,
            root_pem: generated.root,
            crl: None,
        })
    }

    pub(crate) fn enable_crl(&mut self) -> Result {
        let pem = self.pki.crl_pem().map_err(|error| error.to_string())?;
        let path = self.state.join("intermediate.crl.pem");
        std::fs::write(&path, pem)?;
        self.crl = Some(path);
        Ok(())
    }

    pub(crate) async fn serve(self) -> Result<Lab> {
        let issuer = Issuer::from_ca_cert_pem(
            &self.intermediate,
            KeyPair::from_pem(&self.intermediate_key)?,
        )?;
        let server_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
        let mut params =
            CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])?;
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let certificate = params.signed_by(&server_key, &issuer)?;
        let cert_path = self.directory.path().join("server.pem");
        let key_path = self.directory.path().join("server.key");
        std::fs::write(
            &cert_path,
            format!("{}{}", certificate.pem(), self.intermediate),
        )?;
        std::fs::write(&key_path, server_key.serialize_pem())?;
        let root_path = self.directory.path().join("root.pem");
        std::fs::write(&root_path, &self.root_pem)?;
        let tls = TlsPaths {
            cert: cert_path,
            key: key_path,
            ca: Some(root_path),
            crl: self.crl,
            require_client: true,
        };
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let authority: Arc<dyn CertificateAuthority> = self.pki.clone();
        let store: Arc<dyn kernel::memory::MemoryStore> =
            Arc::new(testkit::FakeMemoryStore::default());
        let gateway = Arc::new(
            gateway::Gateway::new(store, settings()).with_certificate_authority(authority),
        );
        let router = gateway::routes::ca::router().with_state(gateway);
        let cancel = CancellationToken::new();
        let shutdown = cancel.clone();
        let task = tokio::spawn(gateway::server::serve_router_with(
            listener,
            router,
            Some(tls),
            shutdown,
            ServeOptions::default(),
            kernel::shutdown::Activities::new(),
        ));
        Ok(Lab {
            _directory: self.directory,
            pki: self.pki,
            intermediate: self.intermediate,
            root: reqwest::Certificate::from_pem(self.root_pem.as_bytes())?,
            base: format!("https://127.0.0.1:{port}"),
            cancel,
            task: Some(task),
        })
    }

    pub(crate) fn legacy_identity(&self) -> Result<reqwest::Identity> {
        let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
        let issuer = Issuer::from_ca_cert_pem(
            &self.intermediate,
            KeyPair::from_pem(&self.intermediate_key)?,
        )?;
        let mut params = CertificateParams::new(vec!["legacy.example.test".to_owned()])?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "device:laptop");
        params
            .distinguished_name
            .push(rcgen::DnType::OrganizationalUnitName, "client");
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let certificate = params.signed_by(&key, &issuer)?;
        identity_pem(&certificate.pem(), &self.intermediate, &key.serialize_pem())
    }
}

impl Lab {
    pub(crate) fn client(&self, identity: Option<reqwest::Identity>) -> Result<reqwest::Client> {
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .tls_certs_only([self.root.clone()])
            .timeout(Duration::from_secs(30));
        if let Some(identity) = identity {
            builder = builder.identity(identity);
        }
        Ok(builder.build()?)
    }

    pub(crate) async fn get(
        &self,
        path: &str,
        identity: Option<reqwest::Identity>,
    ) -> Result<Reply> {
        let response = tokio::time::timeout(
            Duration::from_secs(35),
            self.client(identity)?.get(self.url(path)).send(),
        )
        .await??;
        reply(response).await
    }

    pub(crate) async fn post_sign(
        &self,
        csr: &str,
        token: Option<&str>,
        identity: Option<reqwest::Identity>,
    ) -> Result<Reply> {
        let mut request = self
            .client(identity)?
            .post(self.url("/ca/v1/sign"))
            .json(&SignRequest {
                csr: csr.to_owned(),
            });
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = tokio::time::timeout(Duration::from_secs(35), request.send()).await??;
        reply(response).await
    }

    pub(crate) async fn stop(mut self) -> Result {
        self.cancel.cancel();
        let task = self.task.take().ok_or("CA server task is already gone")?;
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .map_err(|_| "CA server did not stop")??;
        result.map_err(Into::into)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub(crate) fn csr(uri: &str) -> Result<(String, String)> {
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    let mut params = CertificateParams::default();
    params.subject_alt_names = vec![SanType::URI(uri.try_into()?)];
    Ok((params.serialize_request(&key)?.pem()?, key.serialize_pem()))
}

pub(crate) fn identity_pem(leaf: &str, intermediate: &str, key: &str) -> Result<reqwest::Identity> {
    if !leaf.ends_with('\n') || !intermediate.ends_with('\n') || !key.ends_with('\n') {
        return Err("certificate PEM is missing its trailing newline".into());
    }
    let pem = format!("{leaf}{intermediate}{key}");
    Ok(reqwest::Identity::from_pem(pem.as_bytes())?)
}

pub(crate) fn uri_san(pem: &str) -> Result<String> {
    let der = CertificateDer::from_pem_slice(pem.as_bytes())?;
    let (_, certificate) = x509_parser::prelude::X509Certificate::from_der(der.as_ref())?;
    let san = certificate
        .subject_alternative_name()
        .map_err(|error| error.to_string())?
        .ok_or("leaf has no subject alternative name")?;
    let mut uris = Vec::new();
    for name in &san.value.general_names {
        if let x509_parser::extensions::GeneralName::URI(uri) = name {
            uris.push((*uri).to_owned());
        }
    }
    match uris.as_slice() {
        [uri] => Ok(uri.clone()),
        _ => Err("leaf must contain exactly one URI SAN".into()),
    }
}

pub(crate) fn serial(pem: &str) -> Result<Vec<u8>> {
    let der = CertificateDer::from_pem_slice(pem.as_bytes())?;
    let (_, certificate) = x509_parser::prelude::X509Certificate::from_der(der.as_ref())?;
    Ok(certificate.raw_serial().to_vec())
}

pub(crate) fn crl_serials(pem: &str) -> Result<Vec<Vec<u8>>> {
    let der = CertificateRevocationListDer::from_pem_slice(pem.as_bytes())?;
    let (_, crl) = x509_parser::revocation_list::CertificateRevocationList::from_der(der.as_ref())?;
    Ok(crl
        .iter_revoked_certificates()
        .map(|revoked| revoked.raw_serial().to_vec())
        .collect())
}

fn settings() -> gateway::Settings {
    let scratch = PathBuf::from("/tmp/rr-cli-gateway-ca-unused");
    gateway::Settings {
        hostname: "gold-host".to_owned(),
        node: "golden-node".to_owned(),
        allowed_origins: vec!["https://hub.example.test".to_owned()],
        allowed_hosts: vec!["hub.example.test".to_owned()],
        files_root: None,
        static_dir: None,
        root_redirect: None,
        state_dir: scratch.join("state"),
        directory_root: scratch.join("agents"),
        shared_dir: scratch.join("shared"),
        allowed_harnesses: None,
        devices_enabled: false,
        probe_timeout: Duration::from_millis(200),
        mesh_ca: None,
    }
}

async fn reply(response: reqwest::Response) -> Result<Reply> {
    let status = response.status();
    let content_type = match response.headers().get(reqwest::header::CONTENT_TYPE) {
        Some(value) => Some(value.to_str()?.to_owned()),
        None => None,
    };
    let body = response.text().await?;
    Ok(Reply {
        status,
        content_type,
        body,
    })
}
