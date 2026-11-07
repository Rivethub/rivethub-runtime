use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gateway::tenancy::{Tenancy, UserId};

pub(crate) fn user(name: &str) -> UserId {
    UserId::new(name.into()).expect("test user")
}

pub(crate) struct Fixture {
    pub(crate) gateway: Arc<gateway::Gateway>,
    pub(crate) registry: Arc<gateway::sessions::SessionRegistry>,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        Self::with_registry(gateway::sessions::SessionRegistry::default())
    }

    pub(crate) fn with_registry(registry: gateway::sessions::SessionRegistry) -> Self {
        Self::with_lister(registry, None)
    }

    pub(crate) fn with_lister(
        registry: gateway::sessions::SessionRegistry,
        lister: Option<Arc<dyn kernel::transcript::SessionLister>>,
    ) -> Self {
        Self::build(
            registry,
            Ports {
                lister,
                deletion: None,
                catalog: None,
            },
        )
    }

    pub(crate) fn with_deletion(
        registry: gateway::sessions::SessionRegistry,
        lister: Option<Arc<dyn kernel::transcript::SessionLister>>,
        deletion: Arc<dyn kernel::memory::SessionDeletion>,
    ) -> Self {
        Self::build(
            registry,
            Ports {
                lister,
                deletion: Some(deletion),
                catalog: None,
            },
        )
    }

    pub(crate) fn with_catalog(
        registry: gateway::sessions::SessionRegistry,
        lister: Option<Arc<dyn kernel::transcript::SessionLister>>,
        deletion: Arc<dyn kernel::memory::SessionDeletion>,
        catalog: Arc<dyn kernel::memory::SessionCatalog>,
    ) -> Self {
        Self::build(
            registry,
            Ports {
                lister,
                deletion: Some(deletion),
                catalog: Some(catalog),
            },
        )
    }

    fn build(registry: gateway::sessions::SessionRegistry, ports: Ports) -> Self {
        let registry = Arc::new(registry);
        let tenancy = Tenancy::local(user("owner"))
            .with_token("alice-test-token", user("alice"))
            .expect("token");
        let gateway = gateway::Gateway::new(
            Arc::new(testkit::FakeMemoryStore::default()),
            gateway::Settings {
                hostname: "gold-host".into(),
                node: "golden-node".into(),
                allowed_hosts: vec![],
                allowed_origins: vec![],
                files_root: None,
                static_dir: None,
                root_redirect: None,
                state_dir: PathBuf::from("/tmp/rr-gateway-unused-agents/state"),
                directory_root: PathBuf::from("/tmp/rr-gateway-unused-agents/agents"),
                shared_dir: PathBuf::from("/tmp/rr-gateway-unused-agents/shared"),
                allowed_harnesses: None,
                probe_timeout: Duration::from_millis(200),
                private_ca: None,
            },
        )
        .with_tenancy(tenancy)
        .with_sessions(registry.clone());
        let gateway = Arc::new(ports.wire(gateway));
        Self { gateway, registry }
    }
}

pub(crate) async fn http(
    gateway: &Arc<gateway::Gateway>,
    method: &str,
    path: &str,
    body: impl Into<Vec<u8>>,
    token: Option<&str>,
) -> (u16, Vec<u8>) {
    use tower::ServiceExt as _;
    let mut request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost")
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let mut request = request
        .body(axum::body::Body::from(body.into()))
        .expect("request");
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:1234"
            .parse::<std::net::SocketAddr>()
            .expect("address"),
    ));
    let response = gateway
        .router(false)
        .oneshot(request)
        .await
        .expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    (status, bytes.to_vec())
}

struct Ports {
    lister: Option<Arc<dyn kernel::transcript::SessionLister>>,
    deletion: Option<Arc<dyn kernel::memory::SessionDeletion>>,
    catalog: Option<Arc<dyn kernel::memory::SessionCatalog>>,
}

impl Ports {
    fn wire(self, gateway: gateway::Gateway) -> gateway::Gateway {
        let gateway = match self.lister {
            Some(lister) => gateway.with_session_lister(lister),
            None => gateway,
        };
        let gateway = match self.deletion {
            Some(deletion) => gateway.with_session_deletion(deletion),
            None => gateway,
        };
        match self.catalog {
            Some(catalog) => gateway.with_session_catalog(catalog),
            None => gateway,
        }
    }
}
