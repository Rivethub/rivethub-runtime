use crate::common;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::GatedStore;
use gateway::Gateway;
use gateway::server::BindSource;
use gateway::server::ServeError;
use gateway::server::ServeOptions;
use kernel::memory::MemoryStore;
use tempfile::tempdir;
use tokio::io::AsyncReadExt as _;
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

const HEALTHZ_CLOSE: &[u8] =
    b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
const HEALTHZ_KEEP_ALIVE: &[u8] = b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
const SEARCH_HOLD: &[u8] = b"GET /api/memory/search?q=held HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
const CAPTURE_HEAD: &[u8] = concat!(
    "POST /api/capture HTTP/1.1\r\n",
    "Host: 127.0.0.1\r\n",
    "Content-Type: application/json\r\n",
    "Content-Length: 100\r\n",
    "\r\n"
)
.as_bytes();

type ServerHandle = tokio::task::JoinHandle<Result<(), ServeError>>;

struct TlsMaterial {
    _root: tempfile::TempDir,
    cert: PathBuf,
    key: PathBuf,
    connector: TlsConnector,
}

#[expect(
    clippy::expect_used,
    reason = "test helper; the clippy.toml exemption only reaches #[test] functions"
)]
fn tls_material() -> TlsMaterial {
    let key_pair = rcgen::KeyPair::generate().expect("test key");
    let params =
        rcgen::CertificateParams::new(vec!["127.0.0.1".to_string(), "localhost".to_string()])
            .expect("cert params");
    let certificate = params.self_signed(&key_pair).expect("certificate");
    let root = tempdir().expect("temp root");
    let cert = root.path().join("node.crt");
    let key = root.path().join("node.key");
    std::fs::write(&cert, certificate.pem()).expect("cert written");
    std::fs::write(&key, key_pair.serialize_pem()).expect("key written");
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(certificate.der().clone())
        .expect("self signed root");
    let client_config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    TlsMaterial {
        _root: root,
        cert,
        key,
        connector: TlsConnector::from(Arc::new(client_config)),
    }
}

#[expect(
    clippy::expect_used,
    reason = "test helper; the clippy.toml exemption only reaches #[test] functions"
)]
async fn plain_server_with(options: ServeOptions) -> (SocketAddr, CancellationToken, ServerHandle) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("listener");
    let addr = listener.local_addr().expect("addr");
    let token = CancellationToken::new();
    let (gateway, _) = common::gateway();
    let server = tokio::spawn(gateway::server::serve_with(
        listener,
        gateway,
        None,
        token.clone(),
        options,
    ));
    (addr, token, server)
}

#[expect(
    clippy::expect_used,
    reason = "test helper; the clippy.toml exemption only reaches #[test] functions"
)]
async fn tls_server_with(
    options: ServeOptions,
) -> (SocketAddr, CancellationToken, ServerHandle, TlsMaterial) {
    let material = tls_material();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("listener");
    let addr = listener.local_addr().expect("addr");
    let token = CancellationToken::new();
    let tls = gateway::tls::resolve_tls(&BindSource {
        host: "127.0.0.1".to_string(),
        port: addr.port(),
        tls_cert: Some(material.cert.clone()),
        tls_key: Some(material.key.clone()),
        notices: Vec::new(),
    })
    .expect("tls resolves")
    .expect("tls paths present");
    let (gateway, _) = common::gateway();
    let server = tokio::spawn(gateway::server::serve_with(
        listener,
        gateway,
        Some(tls),
        token.clone(),
        options,
    ));
    (addr, token, server, material)
}

#[expect(
    clippy::expect_used,
    reason = "test helper; the clippy.toml exemption only reaches #[test] functions"
)]
async fn stop(token: CancellationToken, server: ServerHandle) {
    token.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("serve finishes after cancellation")
        .expect("serve task");
    assert!(outcome.is_ok());
}

#[expect(
    clippy::expect_used,
    reason = "test helper; the clippy.toml exemption only reaches #[test] functions"
)]
async fn exchange(addr: SocketAddr, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream.write_all(request).await.expect("request written");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("response read");
    String::from_utf8_lossy(&raw).into_owned()
}

#[expect(
    clippy::expect_used,
    reason = "test helper; the clippy.toml exemption only reaches #[test] functions"
)]
async fn tls_exchange(connector: TlsConnector, addr: SocketAddr) -> String {
    let stream = TcpStream::connect(addr).await.expect("connect");
    let mut tls_stream = connector
        .connect("127.0.0.1".try_into().expect("server name"), stream)
        .await
        .expect("tls handshake");
    tls_stream
        .write_all(HEALTHZ_CLOSE)
        .await
        .expect("request written");
    let mut raw = Vec::new();
    tls_stream
        .read_to_end(&mut raw)
        .await
        .expect("response read");
    String::from_utf8_lossy(&raw).into_owned()
}

#[expect(
    clippy::expect_used,
    reason = "test helper; the clippy.toml exemption only reaches #[test] functions"
)]
async fn read_response(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = stream.read(&mut byte).await.expect("head byte read");
        assert!(read > 0, "connection closed before the response head");
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let length = text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await.expect("body read");
    format!("{text}{}", String::from_utf8_lossy(&body))
}

#[expect(
    clippy::expect_used,
    reason = "test helper; the clippy.toml exemption only reaches #[test] functions"
)]
async fn round_trip(stream: &mut TcpStream, request: &[u8]) -> String {
    stream.write_all(request).await.expect("request written");
    read_response(stream).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serve_answers_healthz_and_shuts_down_on_the_token() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("listener");
    let addr = listener.local_addr().expect("addr");
    let token = CancellationToken::new();
    let (gateway, _) = common::gateway();
    let server = tokio::spawn(gateway::server::serve(
        listener,
        gateway,
        None,
        token.clone(),
    ));
    let text = exchange(addr, HEALTHZ_CLOSE).await;
    assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
    assert!(text.contains("\"node\":\"golden-node\""), "{text}");
    stop(token, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tls_serving_answers_healthz_over_rustls() {
    let (addr, token, server, material) = tls_server_with(ServeOptions::default()).await;
    let text = tokio::time::timeout(
        Duration::from_secs(10),
        tls_exchange(material.connector.clone(), addr),
    )
    .await
    .expect("healthz over rustls");
    assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
    stop(token, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_tls_handshake_does_not_block_other_clients() {
    let (addr, token, server, material) = tls_server_with(ServeOptions::default()).await;
    let stalled = TcpStream::connect(addr).await.expect("stalled connect");
    let text = tokio::time::timeout(
        Duration::from_secs(10),
        tls_exchange(material.connector.clone(), addr),
    )
    .await
    .expect("second client served while the first handshake stalls");
    assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
    drop(stalled);
    stop(token, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_healthz_across_plain_and_tls() {
    let (plain_addr, plain_token, plain_server) = plain_server_with(ServeOptions::default()).await;
    let (tls_addr, tls_token, tls_server, material) =
        tls_server_with(ServeOptions::default()).await;
    let mut clients = Vec::new();
    for _ in 0..32 {
        clients.push(tokio::spawn(exchange(plain_addr, HEALTHZ_CLOSE)));
        clients.push(tokio::spawn(tls_exchange(
            material.connector.clone(),
            tls_addr,
        )));
    }
    for client in clients {
        let text = tokio::time::timeout(Duration::from_secs(10), client)
            .await
            .expect("client finished")
            .expect("client task");
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
    }
    stop(plain_token, plain_server).await;
    stop(tls_token, tls_server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graceful_shutdown_finishes_in_flight_requests() {
    let store = Arc::new(GatedStore::default());
    let handle: Arc<dyn MemoryStore> = Arc::clone(&store) as Arc<dyn MemoryStore>;
    let gateway = Arc::new(Gateway::new(handle, common::settings()));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("listener");
    let addr = listener.local_addr().expect("addr");
    let token = CancellationToken::new();
    let server = tokio::spawn(gateway::server::serve_with(
        listener,
        gateway,
        None,
        token.clone(),
        ServeOptions::default(),
    ));
    let mut entered = store.entered.subscribe();
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(b"GET /api/memory/search?q=hello HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .expect("request written");
    tokio::time::timeout(Duration::from_secs(5), entered.changed())
        .await
        .expect("search entered the store")
        .expect("watch alive");
    token.cancel();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut refused = false;
    while tokio::time::Instant::now() < deadline {
        if TcpStream::connect(addr).await.is_err() {
            refused = true;
            break;
        }
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                sleep,
                reason = "the listener publishes no awaitable refusal event; the 20 ms poll is bounded by the deadline"
            )
        )]
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(refused, "listener still accepting after cancellation");
    store.release.add_permits(1);
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
        .await
        .expect("in-flight request finished")
        .expect("response read");
    let text = String::from_utf8_lossy(&raw);
    assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
    assert!(text.contains("\"query\":\"hello\""), "{text}");
    let outcome = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("serve returns after the last connection drains")
        .expect("serve task");
    assert!(outcome.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_request_head_is_cut_off() {
    let (addr, token, server) = plain_server_with(ServeOptions {
        header_read_timeout: Duration::from_millis(250),
        ..ServeOptions::default()
    })
    .await;
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: 127")
        .await
        .expect("partial head written");
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), stream.read_to_end(&mut raw))
        .await
        .expect("connection closed")
        .expect("read");
    assert!(
        raw.is_empty(),
        "no response for a stalled head: {}",
        String::from_utf8_lossy(&raw)
    );
    stop(token, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_tls_handshake_is_cut_off() {
    let (addr, token, server, _material) = tls_server_with(ServeOptions {
        tls_handshake_timeout: Duration::from_millis(250),
        ..ServeOptions::default()
    })
    .await;
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("handshake wait is bounded")
        .expect("read");
    assert!(
        raw.is_empty(),
        "no handshake bytes: {}",
        String::from_utf8_lossy(&raw)
    );
    stop(token, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trickled_request_body_hits_the_request_deadline() {
    let (addr, token, server) = plain_server_with(ServeOptions {
        request_timeout: Duration::from_secs(1),
        ..ServeOptions::default()
    })
    .await;
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let started = tokio::time::Instant::now();
    stream.write_all(CAPTURE_HEAD).await.expect("head written");
    let (mut reader, mut writer) = tokio::io::split(stream);
    let trickle = tokio::spawn(async move {
        for _ in 0..100 {
            if writer.write_all(b"a").await.is_err() {
                break;
            }
            #[cfg_attr(
                dylint_lib = "rr_lints",
                expect(
                    sleep,
                    reason = "the request deadline is exercised by a body that trickles; the 200 ms gaps are the fixture"
                )
            )]
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), reader.read_to_end(&mut raw))
        .await
        .expect("the deadline cuts the trickle off")
        .expect("response read to EOF");
    let elapsed = started.elapsed();
    trickle.abort();
    if let Err(error) = trickle.await {
        assert!(error.is_cancelled(), "{error}");
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    assert!(text.starts_with("HTTP/1.1 408"), "{text}");
    assert!(text.contains("\"error\":\"request timeout\""), "{text}");
    assert!(elapsed >= Duration::from_millis(900), "{elapsed:?}");
    stop(token, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keep_alive_reuse_serves_a_second_request_inside_the_idle_window() {
    let (addr, token, server) = plain_server_with(ServeOptions {
        keep_alive_idle_timeout: Duration::from_millis(250),
        ..ServeOptions::default()
    })
    .await;
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let first = round_trip(&mut stream, HEALTHZ_KEEP_ALIVE).await;
    assert!(first.starts_with("HTTP/1.1 200 OK"), "{first}");
    let second = round_trip(&mut stream, HEALTHZ_KEEP_ALIVE).await;
    assert!(second.starts_with("HTTP/1.1 200 OK"), "{second}");
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut raw))
        .await
        .expect("the idle window closes the connection")
        .expect("eof");
    assert!(
        raw.is_empty(),
        "no trailing bytes: {}",
        String::from_utf8_lossy(&raw)
    );
    stop(token, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn held_request_survives_the_keep_alive_idle_window() {
    let store = Arc::new(GatedStore::default());
    let handle: Arc<dyn MemoryStore> = Arc::clone(&store) as Arc<dyn MemoryStore>;
    let gateway = Arc::new(Gateway::new(handle, common::settings()));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("listener");
    let addr = listener.local_addr().expect("addr");
    let token = CancellationToken::new();
    let server = tokio::spawn(gateway::server::serve_with(
        listener,
        gateway,
        None,
        token.clone(),
        ServeOptions {
            keep_alive_idle_timeout: Duration::from_millis(250),
            ..ServeOptions::default()
        },
    ));
    let mut entered = store.entered.subscribe();
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let first = round_trip(&mut stream, HEALTHZ_KEEP_ALIVE).await;
    assert!(first.starts_with("HTTP/1.1 200 OK"), "{first}");
    stream
        .write_all(SEARCH_HOLD)
        .await
        .expect("held request written");
    tokio::time::timeout(Duration::from_secs(5), entered.changed())
        .await
        .expect("search entered the store")
        .expect("watch alive");
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            sleep,
            reason = "the assertion is that the connection outlives the configured 250 ms idle window; the wait must exceed it"
        )
    )]
    tokio::time::sleep(Duration::from_millis(400)).await;
    let mut probe = [0u8; 1];
    let survival = tokio::time::timeout(Duration::from_millis(150), stream.read(&mut probe)).await;
    assert!(
        survival.is_err(),
        "connection was closed while a request was in flight"
    );
    store.release.add_permits(1);
    let text = tokio::time::timeout(Duration::from_secs(5), read_response(&mut stream))
        .await
        .expect("held request answered");
    assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
    assert!(text.contains("\"query\":\"held\""), "{text}");
    stop(token, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipelined_request_held_past_the_idle_window_is_not_dropped() {
    let store = Arc::new(GatedStore::default());
    let handle: Arc<dyn MemoryStore> = Arc::clone(&store) as Arc<dyn MemoryStore>;
    let gateway = Arc::new(Gateway::new(handle, common::settings()));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("listener");
    let addr = listener.local_addr().expect("addr");
    let token = CancellationToken::new();
    let server = tokio::spawn(gateway::server::serve_with(
        listener,
        gateway,
        None,
        token.clone(),
        ServeOptions {
            keep_alive_idle_timeout: Duration::from_millis(250),
            ..ServeOptions::default()
        },
    ));
    let mut entered = store.entered.subscribe();
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let mut pipelined = Vec::new();
    pipelined.extend_from_slice(HEALTHZ_KEEP_ALIVE);
    pipelined.extend_from_slice(SEARCH_HOLD);
    stream
        .write_all(&pipelined)
        .await
        .expect("pipelined requests written in one segment");
    tokio::time::timeout(Duration::from_secs(5), entered.changed())
        .await
        .expect("search entered the store")
        .expect("watch alive");
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            sleep,
            reason = "the assertion is that the pipelined request outlives the configured 250 ms idle window; the wait must exceed it"
        )
    )]
    tokio::time::sleep(Duration::from_millis(400)).await;
    store.release.add_permits(1);
    let healthz = tokio::time::timeout(Duration::from_secs(5), read_response(&mut stream))
        .await
        .expect("pipelined healthz answered on the same connection");
    assert!(healthz.starts_with("HTTP/1.1 200 OK"), "{healthz}");
    assert!(healthz.contains("\"node\":\"golden-node\""), "{healthz}");
    let search = tokio::time::timeout(Duration::from_secs(5), read_response(&mut stream))
        .await
        .expect("pipelined search answered on the same connection");
    assert!(search.starts_with("HTTP/1.1 200 OK"), "{search}");
    assert!(search.contains("\"query\":\"held\""), "{search}");
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut raw))
        .await
        .expect("the idle window closes the connection after both responses")
        .expect("eof");
    assert!(
        raw.is_empty(),
        "no trailing bytes: {}",
        String::from_utf8_lossy(&raw)
    );
    stop(token, server).await;
}
