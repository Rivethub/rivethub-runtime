use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;

use async_trait::async_trait;
use kernel::memory::share::Consent;
use kernel::memory::share::ShareConsent;
use kernel::memory::share::ShareFailure;
use kernel::memory::share::ShareOutcome;
use kernel::memory::share::SharePreview;
use parking_lot::Mutex;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::Sanitization;
use wire::capture::SessionOrigin;
use wire::message::Role;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn bundle(content: &str, sanitized: bool) -> TestResult<Vec<u8>> {
    let session = BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: "desk".to_owned(),
            conversation_id: "00000000-0000-4000-8000-000000000001".to_owned(),
            session_key: "claude-code:one".to_owned(),
            agent: "rivet".to_owned(),
            created_at: None,
            updated_at: None,
            identity: None,
        },
        batch: CaptureBatchRequest {
            session_key: "claude-code:one".to_owned(),
            agent: "rivet".to_owned(),
            channel: None,
            title: Some("set up AcmeKit".to_owned()),
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![CaptureMessage {
                event_id: "evt-1".to_owned(),
                role: Role::User,
                content: content.to_owned(),
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: None,
                created_at: None,
            }],
        },
    };
    let author = BundleAuthor {
        user: "phil".to_owned(),
        node: "desk".to_owned(),
    };
    let raw = capture::bundle::seal(
        author,
        chrono::DateTime::UNIX_EPOCH,
        Sanitization::Raw,
        vec![session],
    )?;
    if !sanitized {
        return Ok(raw.text);
    }
    let policy = capture::sanitize::Policy {
        mapping: capture::sanitize::mapping::Mapping::default(),
        key: capture::sanitize::LocalKey::from_bytes([7; 32]),
        sessions: capture::sanitize::SessionRules::default(),
    };
    Ok(
        capture::sanitize::run(&raw.bundle, &policy, chrono::DateTime::UNIX_EPOCH)?
            .sealed
            .text,
    )
}

struct Swapper {
    path: std::path::PathBuf,
    replacement: Vec<u8>,
}

#[async_trait]
impl ShareConsent for Swapper {
    async fn consent(&self, preview: &SharePreview) -> Result<Consent, ShareFailure> {
        assert_eq!(preview.titles, ["set up AcmeKit"]);
        std::fs::write(&self.path, &self.replacement)?;
        Ok(Consent::Share { unreviewed: 0 })
    }
}

#[tokio::test]
async fn share_seals_the_bytes_the_human_confirmed() -> TestResult {
    let uploads: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let seen = Arc::clone(&uploads);
    let app = axum::Router::new().route(
        "/api/drops",
        axum::routing::post(move |body: axum::body::Bytes| async move {
            seen.lock().push(body.to_vec());
            (
                axum::http::StatusCode::CREATED,
                r#"{"id":"AAAAAAAAAAAAAAAAAAAAAA","expires_at":"2026-11-17T12:00:00Z","bytes":1}"#,
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let cloud = format!("http://{}/", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let home = tempfile::tempdir()?;
    let state = home.path().join(".rivethub");
    std::fs::create_dir_all(&state)?;
    let database = state.join("memory.sqlite");
    migrate::sqlite(&database)?;
    std::fs::write(
        state.join("config.yaml"),
        format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n  share:\n    cloud:\n      url: {cloud}\ngateway:\n  host: 127.0.0.1\n  port: 9\n",
            database.display()
        ),
    )?;
    let tokens = home.path().join(".local/state/rivethub/cloud.json");
    std::fs::create_dir_all(tokens.parent().ok_or("parent")?)?;
    std::fs::write(
        &tokens,
        format!(
            r#"{{"url":"{cloud}","access_token":"rhca_throwaway","refresh_token":"rhcr_throwaway","expires_at":"2099-01-01T00:00:00Z","scope":"drops:write"}}"#
        ),
    )?;
    std::fs::set_permissions(&tokens, std::fs::Permissions::from_mode(0o600))?;
    let env = config::Environment::from_vars([
        (OsStr::new("HOME"), home.path().as_os_str()),
        (OsStr::new("RIVETHUB_NODE_NAME"), OsStr::new("desk")),
    ]);
    let loaded = config::Config::load(
        &env,
        &config::ConfigLocation::Current(state.join("config.yaml")),
        config::Profile::Standalone,
    )?;
    let store = memory_sqlite::SqliteStore::open(&database)?;
    let kit = rivethub::sessions::kit(
        &loaded.config,
        &env,
        kernel::owner::Owner::new("phil".to_owned())?,
        cli::memory::SessionStore::Sqlite(store),
    )?;

    let path = home.path().join("work.clean.json");
    let confirmed = bundle("the plan the human read", true)?;
    std::fs::write(&path, &confirmed)?;
    let swapper = Swapper {
        path: path.clone(),
        replacement: bundle("a swapped raw bundle", false)?,
    };
    let outcome = kit.share.share(path.clone(), &swapper).await?;
    let ShareOutcome::Shared { line, .. } = outcome else {
        return Err("not shared".into());
    };
    let code: capture::seal::ShareCode = line.split_whitespace().last().ok_or("code")?.parse()?;
    let uploaded = uploads.lock().first().cloned().ok_or("an upload")?;
    let opened = capture::seal::open(uploaded, &code.key, &capture::sign::TrustStore::default())?;
    assert_eq!(
        opened.bundle,
        capture::bundle::parse(std::str::from_utf8(&confirmed)?)?,
        "the upload is the bundle the preview showed"
    );
    server.abort();
    Ok(())
}
