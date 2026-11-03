use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use http_body::Frame;
use http_body::SizeHint;
use hyper::Request;
use hyper::Response;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::rt::TokioTimer;
use hyper_util::service::TowerToHyperService;
use kernel::shutdown::Activities;
use parking_lot::Mutex;
use thiserror::Error;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::ReadBuf;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tokio_util::either::Either;
use tokio_util::sync::CancellationToken;

use crate::Gateway;
use crate::tls::TlsError;
use crate::tls::TlsPaths;
use crate::tls::listener_tls_config;

const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindSource {
    pub host: String,
    pub port: u16,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub tls_ca: Option<PathBuf>,
    pub tls_crl: Option<PathBuf>,
    pub require_client: bool,
    pub notices: Vec<String>,
}

#[derive(Debug, Error)]
pub enum BindError {
    #[error(
        "[rivethub-gateway] refusing to bind {host}: gateway TLS is required off-loopback (set RIVETHUB_TLS_CERT + RIVETHUB_TLS_KEY to the node's holder certificate, ~/.rivethub/tls/holder.cert.pem and holder.key.pem from rh mesh enroll; devices enroll with rh cert renew). Or bind the gateway host to 127.0.0.1."
    )]
    TlsRequired { host: String },
    #[error(
        "[rivethub-gateway] TLS CA chain missing at {} — cannot verify device client certs",
        path.display()
    )]
    CaMissing { path: PathBuf },
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum ServeError {
    #[error(transparent)]
    Tls(#[from] TlsError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("TLS configuration worker failed")]
    TlsWorker(#[from] tokio::task::JoinError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServeOptions {
    pub header_read_timeout: Duration,
    pub request_timeout: Duration,
    pub keep_alive_idle_timeout: Duration,
    pub tls_handshake_timeout: Duration,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "the defaults are the Node 22 server values, not the derive's zeros"
    )
)]
impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            header_read_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(300),
            keep_alive_idle_timeout: Duration::from_secs(5),
            tls_handshake_timeout: Duration::from_secs(120),
        }
    }
}

#[must_use]
pub fn is_bind_loopback(host: &str) -> bool {
    matches!(
        host.trim().to_ascii_lowercase().as_str(),
        "127.0.0.1" | "::1" | "localhost" | "::ffff:127.0.0.1"
    )
}

pub async fn bind(source: &BindSource) -> Result<TcpListener, BindError> {
    Ok(TcpListener::bind((source.host.as_str(), source.port)).await?)
}

pub async fn serve(
    listener: TcpListener,
    gateway: Arc<Gateway>,
    tls: Option<TlsPaths>,
    shutdown: CancellationToken,
) -> Result<(), ServeError> {
    serve_with(listener, gateway, tls, shutdown, ServeOptions::default()).await
}

pub async fn serve_with(
    listener: TcpListener,
    gateway: Arc<Gateway>,
    tls: Option<TlsPaths>,
    shutdown: CancellationToken,
    options: ServeOptions,
) -> Result<(), ServeError> {
    let serving = serve_router_with(
        listener,
        gateway.router(tls.is_some()),
        tls,
        shutdown.clone(),
        options,
        Activities::new(),
    );
    tokio::pin!(serving);
    tokio::select! {
        result = &mut serving => { gateway.close().await; result },
        () = shutdown.cancelled() => { gateway.close().await; serving.await },
    }
}

pub async fn serve_router_with(
    listener: TcpListener,
    router: Router,
    tls: Option<TlsPaths>,
    shutdown: CancellationToken,
    options: ServeOptions,
    activities: Activities,
) -> Result<(), ServeError> {
    let acceptor = match tls {
        Some(paths) => {
            let config = tokio::task::spawn_blocking(move || listener_tls_config(&paths)).await??;
            Some(TlsAcceptor::from(Arc::new(config)))
        }
        None => None,
    };
    serve_router_with_acceptor(listener, router, acceptor, shutdown, options, activities).await
}

pub(crate) async fn serve_router_with_acceptor(
    listener: TcpListener,
    router: Router,
    acceptor: Option<TlsAcceptor>,
    shutdown: CancellationToken,
    options: ServeOptions,
    activities: Activities,
) -> Result<(), ServeError> {
    let serving = Serving {
        options,
        router,
        acceptor,
        shutdown: shutdown.clone(),
        activities,
    };
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((tcp, remote)) => {
                    if let Err(error) = tcp.set_nodelay(true) {
                        tracing::debug!("tcp nodelay failed for {remote}: {error}");
                    }
                    tasks.spawn(serving.clone().run(tcp, remote));
                }
                Err(error) => {
                    tracing::warn!("gateway accept failed: {error}");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            }
        }
    }
    drop(listener);
    while let Some(joined) = tasks.join_next().await {
        if let Err(error) = joined {
            tracing::debug!("gateway connection task ended: {error}");
        }
    }
    Ok(())
}

#[derive(Clone)]
struct Serving {
    options: ServeOptions,
    router: Router,
    acceptor: Option<TlsAcceptor>,
    shutdown: CancellationToken,
    activities: Activities,
}

impl Serving {
    async fn run(self, tcp: TcpStream, remote: SocketAddr) {
        let _connection = self
            .activities
            .enter(format!("gateway connection {remote}"));
        let stream = match self.acceptor {
            Some(acceptor) => {
                let handshake =
                    tokio::time::timeout(self.options.tls_handshake_timeout, acceptor.accept(tcp));
                tokio::pin!(handshake);
                tokio::select! {
                    outcome = &mut handshake => match outcome {
                        Ok(Ok(tls)) => Either::Right(tls),
                        Ok(Err(error)) => {
                            tracing::debug!("tls handshake from {remote} failed: {error}");
                            return;
                        }
                        Err(_) => {
                            tracing::debug!("tls handshake from {remote} timed out");
                            return;
                        }
                    },
                    () = self.shutdown.cancelled() => return,
                }
            }
            None => Either::Left(tcp),
        };
        let peer = peer_certificate(&stream);
        let keep_alive = KeepAliveGate::new(self.options.keep_alive_idle_timeout);
        let clock = RequestClock::default();
        let socket_gone = CancellationToken::new();
        let service = TowerToHyperService::new(GatewayService {
            router: self.router,
            remote,
            peer,
            keep_alive: keep_alive.clone(),
            clock: clock.clone(),
            request_timeout: self.options.request_timeout,
        });
        let mut builder = http1::Builder::new();
        builder
            .timer(TokioTimer::new())
            .header_read_timeout(self.options.header_read_timeout);
        {
            let mut connection = pin!(
                builder
                    .serve_connection(
                        TokioIo::new(ClientActivity {
                            inner: stream,
                            keep_alive: keep_alive.clone(),
                            clock,
                            socket_gone: socket_gone.clone(),
                        }),
                        service,
                    )
                    .with_upgrades()
            );
            let idle = keep_alive_watchdog(keep_alive.receiver());
            let mut idle = pin!(idle);
            tokio::select! {
                outcome = connection.as_mut() => {
                    if let Err(error) = outcome {
                        tracing::debug!("connection from {remote} ended: {error}");
                    }
                }
                () = idle.as_mut() => {
                    tracing::debug!("closing idle keep-alive connection from {remote}");
                }
                () = self.shutdown.cancelled() => {
                    connection.as_mut().graceful_shutdown();
                    if let Err(error) = connection.as_mut().await {
                        tracing::debug!("connection from {remote} ended: {error}");
                    }
                }
            }
        }
        socket_gone.cancelled().await;
    }
}

#[derive(Clone)]
struct GatewayService {
    router: Router,
    remote: SocketAddr,
    peer: Option<crate::tenancy::PeerCertificate>,
    keep_alive: KeepAliveGate,
    clock: RequestClock,
    request_timeout: Duration,
}

fn peer_certificate(
    stream: &Either<TcpStream, tokio_rustls::server::TlsStream<TcpStream>>,
) -> Option<crate::tenancy::PeerCertificate> {
    match stream {
        Either::Right(tls) => tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|certs| certs.first())
            .map(|cert| crate::tenancy::PeerCertificate::from_verified_leaf(cert.as_ref())),
        Either::Left(_) => None,
    }
}

impl tower::Service<Request<Incoming>> for GatewayService {
    type Response = Response<KeepAliveBody>;
    type Error = std::convert::Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: Request<Incoming>) -> Self::Future {
        self.keep_alive.disarm();
        request.extensions_mut().insert(ConnectInfo(self.remote));
        if let Some(peer) = &self.peer {
            request.extensions_mut().insert(peer.clone());
        }
        let start = self.clock.start();
        let request = request.map(|body| DeadlineBody::new(body, start, self.request_timeout));
        let routed = self.router.call(request);
        let keep_alive = self.keep_alive.clone();
        let clock = self.clock.clone();
        Box::pin(async move {
            let response = match routed.await {
                Ok(response) => response,
                Err(never) => match never {},
            };
            let arm = if response.status() == StatusCode::SWITCHING_PROTOCOLS {
                None
            } else {
                Some(keep_alive)
            };
            Ok(response.map(|body| KeepAliveBody {
                inner: body,
                keep_alive: arm,
                clock,
            }))
        })
    }
}

#[derive(Debug, Error)]
#[error("request did not finish within the deadline")]
pub struct RequestDeadlineExceeded;

struct DeadlineBody {
    inner: Pin<Box<Incoming>>,
    expiry: Pin<Box<tokio::time::Sleep>>,
}

impl DeadlineBody {
    fn new(inner: Incoming, start: Option<tokio::time::Instant>, timeout: Duration) -> Self {
        let at = start.unwrap_or_else(tokio::time::Instant::now) + timeout;
        Self {
            inner: Box::pin(inner),
            expiry: Box::pin(tokio::time::sleep_until(at)),
        }
    }
}

impl http_body::Body for DeadlineBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.expiry.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Some(Err(axum::Error::new(RequestDeadlineExceeded))));
        }
        this.inner
            .as_mut()
            .poll_frame(cx)
            .map(|frame| frame.map(|result| result.map_err(axum::Error::new)))
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

struct KeepAliveBody {
    inner: axum::body::Body,
    keep_alive: Option<KeepAliveGate>,
    clock: RequestClock,
}

impl http_body::Body for KeepAliveBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Pin::new(&mut self.get_mut().inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for KeepAliveBody {
    fn drop(&mut self) {
        self.clock.complete();
        if let Some(keep_alive) = self.keep_alive.as_ref() {
            keep_alive.arm();
        }
    }
}

#[derive(Debug, Default)]
struct RequestStart {
    first_byte: Option<tokio::time::Instant>,
    in_flight: bool,
}

#[derive(Clone, Default)]
struct RequestClock(Arc<Mutex<RequestStart>>);

impl RequestClock {
    fn first_byte(&self) {
        let mut start = self.0.lock();
        if !start.in_flight {
            start.first_byte = Some(tokio::time::Instant::now());
            start.in_flight = true;
        }
    }

    fn start(&self) -> Option<tokio::time::Instant> {
        let mut start = self.0.lock();
        start.in_flight = true;
        start.first_byte.take()
    }

    fn complete(&self) {
        let mut start = self.0.lock();
        start.first_byte = None;
        start.in_flight = false;
    }
}

struct ClientActivity<S> {
    inner: S,
    keep_alive: KeepAliveGate,
    clock: RequestClock,
    socket_gone: CancellationToken,
}

impl<S> Drop for ClientActivity<S> {
    fn drop(&mut self) {
        self.socket_gone.cancel();
    }
}

impl<S> AsyncRead for ClientActivity<S>
where
    S: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let filled = buf.filled().len();
        let outcome = Pin::new(&mut this.inner).poll_read(cx, buf);
        if matches!(outcome, Poll::Ready(Ok(()))) && buf.filled().len() > filled {
            this.keep_alive.disarm();
            this.clock.first_byte();
        }
        outcome
    }
}

impl<S> AsyncWrite for ClientActivity<S>
where
    S: AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[derive(Clone)]
struct KeepAliveGate {
    idle: Duration,
    deadline: Arc<watch::Sender<Option<tokio::time::Instant>>>,
}

impl KeepAliveGate {
    fn new(idle: Duration) -> Self {
        Self {
            idle,
            deadline: Arc::new(watch::channel(None).0),
        }
    }

    fn receiver(&self) -> watch::Receiver<Option<tokio::time::Instant>> {
        self.deadline.subscribe()
    }

    fn arm(&self) {
        let at = tokio::time::Instant::now() + self.idle;
        self.deadline.send_if_modified(|armed| {
            if armed.is_some_and(|current| current == at) {
                false
            } else {
                *armed = Some(at);
                true
            }
        });
    }

    fn disarm(&self) {
        self.deadline
            .send_if_modified(|armed| armed.take().is_some());
    }
}

async fn keep_alive_watchdog(mut deadline: watch::Receiver<Option<tokio::time::Instant>>) {
    loop {
        let Some(armed) = *deadline.borrow_and_update() else {
            if deadline.changed().await.is_err() {
                return;
            }
            continue;
        };
        if armed <= tokio::time::Instant::now() {
            return;
        }
        tokio::select! {
            changed = deadline.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            () = tokio::time::sleep_until(armed) => {
                if deadline
                    .borrow_and_update()
                    .is_some_and(|current| current <= tokio::time::Instant::now())
                {
                    return;
                }
            }
        }
    }
}
