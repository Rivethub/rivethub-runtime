use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use gateway::tenancy::{Tenancy, UserId};
use gateway::term::{Delegate, LaunchCommand, TerminalOptions, TerminalService};
use kernel::terminal::{
    PtyInfo, TermSize, TerminalAttachment, TerminalDetails, TerminalId, TerminalMux,
    TerminalMuxError,
};
use parking_lot::Mutex;
use tokio::sync::{broadcast, mpsc, watch};
use wire::harness::HarnessId;
use wire::session_id::SessionId;

pub(crate) mod harness;

pub(crate) const WAIT: Duration = Duration::from_secs(5);

pub(crate) fn user(name: &str) -> UserId {
    UserId::new(name.into()).expect("test user")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Call {
    Spawn {
        session: SessionId,
        argv: Vec<String>,
        cwd: PathBuf,
        size: TermSize,
    },
    Input(Vec<u8>),
    Resize(TermSize),
    Kill,
}

#[derive(Debug)]
struct Terminal {
    session: SessionId,
    size: TermSize,
    output: broadcast::Sender<Vec<u8>>,
    exit: watch::Sender<Option<i32>>,
    replay: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct Mux {
    terminals: Mutex<BTreeMap<TerminalId, Terminal>>,
    turns: Mutex<BTreeMap<TerminalId, mpsc::Sender<kernel::terminal::PaneTurn>>>,
    pub(crate) calls: watch::Sender<Vec<Call>>,
    pub(crate) tasks: tokio_util::task::TaskTracker,
}

impl Default for Mux {
    fn default() -> Self {
        Self {
            terminals: Mutex::new(BTreeMap::new()),
            turns: Mutex::new(BTreeMap::new()),
            calls: watch::channel(Vec::new()).0,
            tasks: tokio_util::task::TaskTracker::new(),
        }
    }
}

impl Mux {
    pub(crate) fn emit(&self, id: &TerminalId, bytes: Vec<u8>) {
        let mut terminals = self.terminals.lock();
        let terminal = terminals.get_mut(id).expect("terminal exists");
        terminal.replay.extend_from_slice(&bytes);
        terminal.output.send(bytes).expect("attached client");
        drop(terminals);
    }

    pub(crate) fn exit(&self, id: &TerminalId, code: i32) {
        self.terminals
            .lock()
            .get(id)
            .expect("terminal exists")
            .exit
            .send_replace(Some(code));
    }

    pub(crate) async fn turn_ended(&self, id: &TerminalId) {
        let turns = self.turns.lock().get(id).cloned();
        turns
            .expect("the terminal's turn ends are followed")
            .send(kernel::terminal::PaneTurn::Ended)
            .await
            .expect("turn end delivered");
    }

    pub(crate) async fn called(&self, predicate: impl Fn(&[Call]) -> bool) {
        let mut calls = self.calls.subscribe();
        tokio::time::timeout(WAIT, async {
            loop {
                if predicate(&calls.borrow_and_update()) {
                    return;
                }
                calls.changed().await.expect("call stream");
            }
        })
        .await
        .expect("port call observed");
    }
}

#[async_trait]
impl TerminalMux for Mux {
    async fn spawn(
        &self,
        session: &SessionId,
        argv: Vec<String>,
        cwd: &Path,
        _env: &BTreeMap<String, String>,
        size: TermSize,
    ) -> Result<TerminalId, TerminalMuxError> {
        let mut terminals = self.terminals.lock();
        let id = TerminalId::new(format!("pty-{}", terminals.len() + 1));
        terminals.insert(
            id.clone(),
            Terminal {
                session: session.clone(),
                size,
                output: broadcast::channel(32).0,
                exit: watch::channel(None).0,
                replay: b"AcmeKit harness ready\r\nacme> ".to_vec(),
            },
        );
        drop(terminals);
        self.calls.send_modify(|calls| {
            calls.push(Call::Spawn {
                session: session.clone(),
                argv,
                cwd: cwd.to_path_buf(),
                size,
            });
        });
        Ok(id)
    }

    async fn attach(&self, id: &TerminalId) -> Result<mpsc::Receiver<Vec<u8>>, TerminalMuxError> {
        Ok(self.open(id).await?.output)
    }

    async fn turn_ends(
        &self,
        id: &TerminalId,
    ) -> Result<mpsc::Receiver<kernel::terminal::PaneTurn>, TerminalMuxError> {
        let (sender, ends) = mpsc::channel(4);
        self.turns.lock().insert(id.clone(), sender);
        Ok(ends)
    }

    async fn inspect(&self, id: &TerminalId) -> Result<TerminalDetails, TerminalMuxError> {
        let terminals = self.terminals.lock();
        let terminal = terminals
            .get(id)
            .ok_or_else(|| TerminalMuxError::UnknownTerminal(id.clone()))?;
        let details = TerminalDetails {
            pid: Some(42),
            size: terminal.size,
            exit_code: *terminal.exit.borrow(),
        };
        drop(terminals);
        Ok(details)
    }

    async fn open(&self, id: &TerminalId) -> Result<TerminalAttachment, TerminalMuxError> {
        let (replay, mut output, mut exit) = {
            let terminals = self.terminals.lock();
            let terminal = terminals
                .get(id)
                .ok_or_else(|| TerminalMuxError::UnknownTerminal(id.clone()))?;
            let subscribed = (
                terminal.replay.clone(),
                terminal.output.subscribe(),
                terminal.exit.subscribe(),
            );
            drop(terminals);
            subscribed
        };
        let (tx, rx) = mpsc::channel(32);
        self.tasks.spawn(async move {
            if exit.borrow().is_some() {
                return;
            }
            loop {
                tokio::select! {
                    () = tx.closed() => break,
                    changed = exit.changed() => { changed.expect("exit stream"); break; },
                    data = output.recv() => {
                        let data = data.expect("terminal output");
                        if tx.send(data).await.is_err() { break; }
                    }
                }
            }
        });
        Ok(TerminalAttachment { replay, output: rx })
    }

    async fn wait(&self, id: &TerminalId) -> Result<i32, TerminalMuxError> {
        let mut exit = self
            .terminals
            .lock()
            .get(id)
            .ok_or_else(|| TerminalMuxError::UnknownTerminal(id.clone()))?
            .exit
            .subscribe();
        loop {
            let code = *exit.borrow_and_update();
            if let Some(code) = code {
                return Ok(code);
            }
            exit.changed()
                .await
                .map_err(|error| TerminalMuxError::Unavailable(Box::new(error)))?;
        }
    }

    async fn input(&self, _id: &TerminalId, bytes: Vec<u8>) -> Result<(), TerminalMuxError> {
        self.calls
            .send_modify(|calls| calls.push(Call::Input(bytes)));
        Ok(())
    }

    async fn resize(&self, id: &TerminalId, size: TermSize) -> Result<(), TerminalMuxError> {
        self.terminals
            .lock()
            .get_mut(id)
            .ok_or_else(|| TerminalMuxError::UnknownTerminal(id.clone()))?
            .size = size;
        self.calls
            .send_modify(|calls| calls.push(Call::Resize(size)));
        Ok(())
    }

    async fn kill(&self, id: &TerminalId) -> Result<(), TerminalMuxError> {
        self.exit(id, 0);
        self.calls.send_modify(|calls| calls.push(Call::Kill));
        Ok(())
    }

    async fn list(&self) -> Result<Vec<PtyInfo>, TerminalMuxError> {
        Ok(self
            .terminals
            .lock()
            .iter()
            .map(|(id, terminal)| PtyInfo {
                id: id.clone(),
                session: terminal.session.clone(),
                running: terminal.exit.borrow().is_none(),
            })
            .collect())
    }
}

pub(crate) struct Fixture {
    pub(crate) gateway: Arc<gateway::Gateway>,
    pub(crate) service: Arc<TerminalService>,
    pub(crate) registry: Arc<gateway::sessions::SessionRegistry>,
    pub(crate) mux: Arc<Mux>,
    pub(crate) directory: tempfile::TempDir,
}

fn launch(
    root: &Path,
    id: &str,
    label: &str,
    session: bool,
    harness: Option<HarnessId>,
) -> LaunchCommand {
    LaunchCommand {
        id: id.into(),
        label: label.into(),
        argv: vec![id.into()],
        cwd: root.to_path_buf(),
        harness,
        session,
        env: BTreeMap::new(),
    }
}

fn terminal(
    service: TerminalService,
    harness: Option<Arc<dyn kernel::harness::HarnessAdapter>>,
    lister: Option<Arc<dyn kernel::transcript::SessionLister>>,
) -> Arc<TerminalService> {
    let service = match harness {
        Some(harness) => service.with_harness(harness),
        None => service,
    };
    Arc::new(match lister {
        Some(lister) => service.with_session_lister(lister),
        None => service,
    })
}

fn uploads(directory: &Path) -> gateway::uploads::UploadService {
    gateway::uploads::UploadService::new(gateway::uploads::UploadOptions {
        root: directory.join("uploads"),
        max_bytes: 1024 * 1024,
        ttl: Duration::from_secs(600),
    })
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        Self::with_harness(None).await
    }

    pub(crate) async fn with_harness(
        harness: Option<Arc<dyn kernel::harness::HarnessAdapter>>,
    ) -> Self {
        Self::with_registry(
            harness,
            gateway::sessions::SessionRegistry::new(Duration::from_secs(1)),
        )
        .await
    }

    pub(crate) async fn with_registry(
        harness: Option<Arc<dyn kernel::harness::HarnessAdapter>>,
        registry: gateway::sessions::SessionRegistry,
    ) -> Self {
        Self::with_lister(harness, registry, None, &[]).await
    }

    pub(crate) async fn with_lister(
        harness: Option<Arc<dyn kernel::harness::HarnessAdapter>>,
        registry: gateway::sessions::SessionRegistry,
        lister: Option<Arc<dyn kernel::transcript::SessionLister>>,
        extra: &[(&str, HarnessId)],
    ) -> Self {
        Self::with_delegates(harness, registry, lister, extra, BTreeMap::new(), None).await
    }

    pub(crate) async fn with_delegates(
        harness: Option<Arc<dyn kernel::harness::HarnessAdapter>>,
        registry: gateway::sessions::SessionRegistry,
        lister: Option<Arc<dyn kernel::transcript::SessionLister>>,
        extra: &[(&str, HarnessId)],
        delegates: BTreeMap<String, Delegate>,
        tasks: Option<Arc<gateway::tasks::Tasks>>,
    ) -> Self {
        let ports = Ports {
            lister,
            deletion: None,
            catalog: None,
            tasks,
        };
        Self::build(harness, registry, ports, extra, delegates).await
    }

    pub(crate) async fn with_deletion(
        harness: Option<Arc<dyn kernel::harness::HarnessAdapter>>,
        registry: gateway::sessions::SessionRegistry,
        lister: Option<Arc<dyn kernel::transcript::SessionLister>>,
        extra: &[(&str, HarnessId)],
        deletion: Arc<dyn kernel::memory::SessionDeletion>,
    ) -> Self {
        let ports = Ports {
            lister,
            deletion: Some(deletion),
            catalog: None,
            tasks: None,
        };
        Self::build(harness, registry, ports, extra, BTreeMap::new()).await
    }

    pub(crate) async fn with_catalog(
        harness: Option<Arc<dyn kernel::harness::HarnessAdapter>>,
        registry: gateway::sessions::SessionRegistry,
        lister: Option<Arc<dyn kernel::transcript::SessionLister>>,
        deletion: Arc<dyn kernel::memory::SessionDeletion>,
        catalog: Arc<dyn kernel::memory::SessionCatalog>,
    ) -> Self {
        let ports = Ports {
            lister,
            deletion: Some(deletion),
            catalog: Some(catalog),
            tasks: None,
        };
        Self::build(harness, registry, ports, &[], BTreeMap::new()).await
    }

    async fn build(
        harness: Option<Arc<dyn kernel::harness::HarnessAdapter>>,
        registry: gateway::sessions::SessionRegistry,
        ports: Ports,
        extra: &[(&str, HarnessId)],
        delegates: BTreeMap<String, Delegate>,
    ) -> Self {
        let directory = tempfile::tempdir().expect("temporary terminal directory");
        let root = directory.path().to_path_buf();
        let registry = Arc::new(registry);
        let mux = Arc::new(Mux::default());
        let commands = [
            ("echo", "Acme Echo", true, None),
            ("shell", "Acme Shell", false, None),
            ("claude", "Claude Stub", true, Some(HarnessId::ClaudeCode)),
            ("silent", "Acme Silent", true, None),
            ("oneshot", "Acme Oneshot", true, None),
            ("envdump", "Acme Env", false, None),
            ("missing", "Missing Binary", false, None),
        ]
        .into_iter()
        .chain(
            extra
                .iter()
                .map(|(id, harness)| (*id, *id, true, Some(*harness))),
        )
        .map(|(id, label, session, harness)| launch(&root, id, label, session, harness))
        .collect();
        let service = TerminalService::new(
            mux.clone(),
            registry.clone(),
            TerminalOptions {
                roots: vec![root],
                default: "echo".into(),
                commands,
                max_ptys: 64,
            },
        )
        .await
        .expect("terminal service");
        let service = terminal(
            service.with_delegates(delegates),
            harness,
            ports.lister.clone(),
        );
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
                mesh_file: Some(PathBuf::from("/no-such-gateway-mesh/mesh.json")),
                shared_dir: PathBuf::from("/tmp/rr-gateway-unused-agents/shared"),
                legacy_mesh_dir: PathBuf::from("/no-such-gateway-legacy"),
                allowed_harnesses: None,
                devices_enabled: false,
                probe_timeout: Duration::from_millis(200),
                mesh_cache_ttl: Duration::ZERO,
                mesh_ca: None,
            },
        )
        .with_tenancy(tenancy)
        .with_sessions(registry.clone())
        .with_terminal(service.clone())
        .with_uploads(uploads(directory.path()));
        let gateway = Arc::new(ports.wire(gateway));
        Self {
            gateway,
            service,
            registry,
            mux,
            directory,
        }
    }

    pub(crate) async fn serve(
        &self,
    ) -> (
        std::net::SocketAddr,
        tokio_util::sync::CancellationToken,
        tokio::task::JoinHandle<Result<(), gateway::server::ServeError>>,
    ) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let stop = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(gateway::server::serve(
            listener,
            self.gateway.clone(),
            None,
            stop.clone(),
        ));
        (address, stop, task)
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

pub(crate) type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub(crate) async fn frame(socket: &mut Socket) -> tokio_tungstenite::tungstenite::Message {
    use futures::StreamExt as _;
    tokio::time::timeout(WAIT, socket.next())
        .await
        .expect("frame timeout")
        .expect("open stream")
        .expect("frame")
}

struct Ports {
    lister: Option<Arc<dyn kernel::transcript::SessionLister>>,
    deletion: Option<Arc<dyn kernel::memory::SessionDeletion>>,
    catalog: Option<Arc<dyn kernel::memory::SessionCatalog>>,
    tasks: Option<Arc<gateway::tasks::Tasks>>,
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
        let gateway = match self.catalog {
            Some(catalog) => gateway.with_session_catalog(catalog),
            None => gateway,
        };
        match self.tasks {
            Some(tasks) => gateway.with_tasks(tasks),
            None => gateway,
        }
    }
}
