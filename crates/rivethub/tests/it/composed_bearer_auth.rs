use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Method;
use axum::http::Request;
use axum::http::StatusCode;
use axum::http::header;
use kernel::memory::MemoryStore;
use rivethub::composition::Composition;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const REMOTE: &str = "192.0.2.7:41000";
const USERS: &str = r#"{"ownerUserId":"owner","unmappedIsOwner":false,"users":{"alice":{"tokens":["49e2e40e591e61357758299c8cee170fb9fa7da160ec8acf110a4a409d905aaf"]}}}"#;

struct Composed {
    router: axum::Router,
    composition: Composition,
    shutdown: CancellationToken,
    _home: tempfile::TempDir,
}

impl Composed {
    async fn build(users: Option<&str>) -> TestResult<Self> {
        let home = tempfile::tempdir()?;
        let work = home.path().join("work");
        let state = home.path().join(".rivethub");
        std::fs::create_dir_all(&work)?;
        std::fs::create_dir_all(&state)?;
        write_config(&state, &work, users)?;
        let env = config::Environment::from_vars([("HOME", home.path().to_str().ok_or("home")?)]);
        let loaded = config::Config::load(
            &env,
            &config::ConfigLocation::Current(state.join("config.yaml")),
        )?;
        let shutdown = CancellationToken::new();
        let store: Arc<dyn MemoryStore> = Arc::new(testkit::FakeMemoryStore::default());
        let composition = tokio::time::timeout(
            Duration::from_secs(30),
            Composition::build(
                &loaded.config,
                &env,
                store,
                &shutdown,
                &kernel::shutdown::Activities::new(),
            ),
        )
        .await??;
        Ok(Self {
            router: composition.router(true),
            composition,
            shutdown,
            _home: home,
        })
    }

    async fn close(self) -> TestResult {
        self.shutdown.cancel();
        self.composition.close().await?;
        Ok(())
    }
}

fn write_config(state: &Path, work: &Path, users: Option<&str>) -> TestResult {
    let users_line = match users {
        Some(body) => {
            let path = state.join("users.json");
            std::fs::write(&path, body)?;
            format!("  users_file: {}\n", path.display())
        }
        None => String::new(),
    };
    let config = format!(
        "runtime:\n  workspace: {work}\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://127.0.0.1:9/v1\n    model: m\n    probe_models: false\ngateway:\n  host: 127.0.0.1\n  port: 5975\n{users_line}",
        work = work.display(),
    );
    std::fs::write(state.join("config.yaml"), config)?;
    Ok(())
}

struct Call<'a> {
    method: Method,
    path: &'a str,
    token: Option<&'a str>,
    headers: &'a [(header::HeaderName, &'a str)],
}

impl<'a> Call<'a> {
    fn get(path: &'a str) -> Self {
        Self {
            method: Method::GET,
            path,
            token: None,
            headers: &[],
        }
    }

    async fn send(self, router: &axum::Router) -> TestResult<axum::response::Response> {
        let mut builder = Request::builder()
            .method(self.method)
            .uri(self.path)
            .header(header::HOST, "127.0.0.1:5975");
        for (name, value) in self.headers {
            builder = builder.header(name, *value);
        }
        if let Some(token) = self.token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let mut request = builder.body(Body::empty())?;
        request
            .extensions_mut()
            .insert(ConnectInfo(REMOTE.parse::<SocketAddr>()?));
        Ok(router.clone().oneshot(request).await?)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_and_a_preflight_answer_a_caller_without_a_token() -> TestResult {
    let composed = Composed::build(None).await?;
    let healthz = Call::get("/healthz").send(&composed.router).await?;
    assert_eq!(healthz.status(), StatusCode::OK);
    let preflight = Call {
        method: Method::OPTIONS,
        path: "/api/sessions",
        token: None,
        headers: &[
            (header::ORIGIN, "app://bundle"),
            (header::ACCESS_CONTROL_REQUEST_METHOD, "GET"),
        ],
    }
    .send(&composed.router)
    .await?;
    assert!(preflight.status().is_success());
    assert_eq!(
        preflight.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some(&axum::http::HeaderValue::from_static("app://bundle"))
    );
    composed.close().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_caller_without_a_token_gets_the_guards_refusal() -> TestResult {
    let composed = Composed::build(None).await?;
    for path in ["/api/sessions", "/api/memory/search?q=x"] {
        let refused = Call {
            method: Method::GET,
            path,
            token: None,
            headers: &[(header::ORIGIN, "app://bundle")],
        }
        .send(&composed.router)
        .await?;
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert!(
            refused
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            "{path} keeps CORS"
        );
    }
    let browser = Call {
        method: Method::GET,
        path: "/wiki",
        token: None,
        headers: &[(header::ACCEPT, "text/html")],
    }
    .send(&composed.router)
    .await?;
    assert_eq!(browser.status(), StatusCode::UNAUTHORIZED);
    let content_type = browser
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or("content type")?;
    assert!(content_type.starts_with("text/html"), "{content_type}");
    composed.close().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_users_file_token_admits_its_user_and_an_unknown_one_is_refused() -> TestResult {
    let composed = Composed::build(Some(USERS)).await?;
    let mut known = Call::get("/api/memory/stats");
    known.token = Some("known-token");
    assert_eq!(known.send(&composed.router).await?.status(), StatusCode::OK);
    let mut unknown = Call::get("/api/memory/stats");
    unknown.token = Some("stranger-token");
    assert_eq!(
        unknown.send(&composed.router).await?.status(),
        StatusCode::UNAUTHORIZED
    );
    composed.close().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_users_file_no_token_is_known() -> TestResult {
    let composed = Composed::build(None).await?;
    let mut call = Call::get("/api/memory/stats");
    call.token = Some("known-token");
    assert_eq!(
        call.send(&composed.router).await?.status(),
        StatusCode::UNAUTHORIZED
    );
    composed.close().await
}
