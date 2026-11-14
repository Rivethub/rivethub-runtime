use std::sync::Arc;
use std::time::Duration;

use cli::Exit;
use gateway::local_owner::OwnerToken;
use testkit::FakeMemoryStore;
use wire::gateway::auth::{SessionRequest, SessionResponse};

use crate::capture_hook::{local_gateway, local_gateway_with};
use crate::support::{self, config, environment, home, install_owner_token, run};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Default)]
struct LogBuffer(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn open_prints_a_fragment_url_whose_code_logs_in_once_and_leaks_nowhere() -> TestResult {
    let logs = LogBuffer::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(logs.clone())
            .finish(),
    );
    let home = home();
    let env = environment(&home);
    let owner = support::local_owner(home.path());
    let store = Arc::new(FakeMemoryStore::default());
    let static_dir = tempfile::tempdir()?;
    std::fs::write(static_dir.path().join("index.html"), "<!doctype html>app")?;
    let (port, stop, server) = local_gateway_with(
        &store,
        home.path(),
        Arc::clone(&owner),
        Some(static_dir.path().to_path_buf()),
    )
    .await?;
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    let report = run(&["open", "--no-browser"], &env).await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    let printed = report.stdout.trim();
    let url = url::Url::parse(printed)?;
    assert_eq!(
        url.origin().ascii_serialization(),
        format!("http://127.0.0.1:{port}")
    );
    assert_eq!(url.path(), "/login");
    assert_eq!(url.query(), None, "{printed}");
    let fragment = url.fragment().ok_or("no fragment")?;
    let code = fragment
        .strip_prefix("code=")
        .ok_or("fragment is not code=")?;
    assert_ne!(code, "");

    let http = reqwest::Client::builder().no_proxy().build()?;
    let exchange = |code: String| {
        let http = http.clone();
        async move {
            http.post(format!("http://127.0.0.1:{port}/api/auth/session"))
                .json(&SessionRequest {
                    code: wire::gateway::auth::Secret::new(code),
                })
                .send()
                .await
        }
    };
    let first = exchange(code.to_owned()).await?;
    assert_eq!(first.status(), 200);
    let cookie = first
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or("no cookie")?;
    let session: SessionResponse = first.json().await?;
    assert_eq!(exchange(code.to_owned()).await?.status(), 401);

    let mut text = report.stderr.clone();
    text.push_str(&String::from_utf8(logs.0.lock().clone())?);
    for secret in [
        owner.current().expose(),
        code,
        session.csrf.expose(),
        cookie.as_str(),
    ] {
        assert!(!text.contains(secret), "leaked into logs or stderr: {text}");
    }
    assert!(
        !report.stdout.contains(owner.current().expose()),
        "token printed"
    );
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await???;
    Ok(())
}

#[tokio::test]
async fn open_on_a_node_with_no_web_app_says_so_and_prints_no_link() -> TestResult {
    let home = home();
    let env = environment(&home);
    let owner = support::local_owner(home.path());
    let store = Arc::new(FakeMemoryStore::default());
    let (port, stop, server) = local_gateway(&store, home.path(), owner).await?;
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    let report = run(&["open", "--no-browser"], &env).await;
    assert_eq!(report.exit, Exit::Failure, "{}", report.stderr);
    assert_eq!(report.stdout, "");
    assert!(
        !report.stdout.contains("http") && !report.stderr.contains("http"),
        "{}",
        report.stderr
    );
    assert!(
        report.stderr.contains("no web app installed"),
        "{}",
        report.stderr
    );
    assert!(report.stderr.contains("MCP"), "{}", report.stderr);
    assert!(report.stderr.contains("preview"), "{}", report.stderr);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await???;
    Ok(())
}

#[tokio::test]
async fn open_without_a_token_file_says_how_to_start_and_prints_no_url() -> TestResult {
    let (port, seen, server) = redirect_stub::plain(axum::http::StatusCode::UNAUTHORIZED).await?;
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    let report = run(&["open", "--no-browser"], &env).await;
    assert_ne!(report.exit, Exit::Ok);
    assert!(!report.stdout.contains("/login#"), "{}", report.stdout);
    assert!(
        report.stderr.contains("401") || report.stderr.contains("token"),
        "{}",
        report.stderr
    );
    drop(seen);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn status_hints_at_rh_open_when_the_gateway_answers_401() -> TestResult {
    let (port, _seen, server) = redirect_stub::plain(axum::http::StatusCode::UNAUTHORIZED).await?;
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    let report = run(&["status"], &env).await;
    let all = format!("{}{}", report.stdout, report.stderr);
    assert!(all.contains("rh open"), "{all}");
    server.abort();
    Ok(())
}

#[tokio::test]
async fn the_owner_header_is_not_forwarded_to_a_different_origin_on_redirect() -> TestResult {
    let (target_port, target_seen, target) =
        redirect_stub::plain(axum::http::StatusCode::OK).await?;
    let (origin_port, origin_seen, origin) = redirect_stub::redirecting(target_port).await?;
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {origin_port}\n"),
    );
    let token = OwnerToken::generate()?;
    install_owner_token(&env, &token);
    let report = run(&["auth", "rotate"], &env).await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    let bearer = format!("Bearer {}", token.expose());
    assert_eq!(origin_seen.lock().as_slice(), [Some(bearer)]);
    assert_eq!(
        target_seen.lock().as_slice(),
        [None],
        "the redirect target saw an Authorization header"
    );
    origin.abort();
    target.abort();
    Ok(())
}

mod redirect_stub {
    use std::sync::Arc;

    pub(super) type Seen = Arc<parking_lot::Mutex<Vec<Option<String>>>>;
    type Stub = Result<(u16, Seen, tokio::task::JoinHandle<()>), std::io::Error>;

    fn record(seen: &Seen, headers: &axum::http::HeaderMap) {
        seen.lock().push(
            headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
        );
    }

    async fn serve(app: axum::Router, seen: Seen) -> Stub {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let server = tokio::spawn(async move {
            drop(axum::serve(listener, app).await);
        });
        Ok((port, seen, server))
    }

    pub(super) async fn plain(status: axum::http::StatusCode) -> Stub {
        let seen = Seen::default();
        let recorder = Arc::clone(&seen);
        let app = axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
            let recorder = Arc::clone(&recorder);
            async move {
                record(&recorder, &headers);
                status
            }
        });
        serve(app, seen).await
    }

    pub(super) async fn redirecting(target: u16) -> Stub {
        let seen = Seen::default();
        let recorder = Arc::clone(&seen);
        let app = axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
            let recorder = Arc::clone(&recorder);
            async move {
                record(&recorder, &headers);
                axum::response::Redirect::temporary(&format!(
                    "http://127.0.0.1:{target}/api/auth/rotate"
                ))
            }
        });
        serve(app, seen).await
    }
}
