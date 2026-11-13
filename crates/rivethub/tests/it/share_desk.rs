use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use axum::response::IntoResponse as _;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::share::Decision;
use kernel::memory::share::SessionChoices;
use kernel::memory::share::desk::DeskError;
use kernel::memory::share::desk::DraftId;
use kernel::memory::share::desk::DraftRequest;
use kernel::memory::share::desk::FindQuery;
use kernel::memory::share::desk::FlagChoice;
use kernel::memory::share::desk::FlagState;
use kernel::memory::share::desk::Mark;
use kernel::memory::share::desk::SendConfirm;
use kernel::memory::share::desk::SendRequest;
use kernel::memory::share::desk::ShareDesk as _;
use parking_lot::Mutex;
use rivethub::share_desk::NodeDesk;
use uuid::Uuid;

use crate::sessions::Node;
use crate::sessions::RISKY;
use crate::sessions::TestResult;
use crate::sessions::call;
use crate::sessions::chosen;
use crate::sessions::decide;
use crate::sessions::sessions;

async fn model() -> TestResult<testkit::Stub> {
    Ok(testkit::stub_server(
        axum::Router::new()
            .route("/v1/decide", axum::routing::post(decide))
            .route(
                "/v1/decide/info",
                axum::routing::get(|| async { r#"{"model": "stub-model"}"# }),
            ),
    )
    .await?)
}

fn desk(node: &Node, block: bool) -> TestResult<NodeDesk> {
    let (config, env) = node.loaded(block)?;
    let store = memory_sqlite::SqliteStore::open(&node.database)?;
    Ok(NodeDesk::new::<rivethub::service::StartupError>(
        &config,
        &env,
        node.owner.clone(),
        cli::memory::SessionStore::Sqlite(store.clone()),
        Arc::new(store),
    )?)
}

fn request(ids: &[&str], decide: bool) -> TestResult<DraftRequest> {
    let sessions: SessionChoices = kernel::memory::share::choices(
        ids.iter()
            .map(|id| Ok((id.parse::<Uuid>()?, Decision::Approve, None)))
            .collect::<Result<Vec<_>, uuid::Error>>()?,
    )?;
    Ok(DraftRequest {
        sessions,
        decide,
        block: false,
    })
}

fn files(root: &Path) -> BTreeMap<PathBuf, (usize, u64)> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
                files.insert(path, (0, 0));
            } else {
                let bytes = std::fs::read(&path).unwrap_or_default();
                let mut hasher = std::hash::DefaultHasher::new();
                std::hash::Hash::hash(&bytes, &mut hasher);
                files.insert(path, (bytes.len(), std::hash::Hasher::finish(&hasher)));
            }
        }
    }
    files
}

fn written(node: &Node) -> Vec<BTreeMap<PathBuf, (usize, u64)>> {
    let home = node.home.path();
    ["/.local/state", "/.local/share", "/tmp"]
        .iter()
        .map(|dir| files(&PathBuf::from(format!("{}{dir}", home.display()))))
        .collect()
}

#[tokio::test]
async fn draft_writes_no_file() -> TestResult {
    let model = model().await?;
    let phil = Node::new("phil", &format!("http://{}", model.addr), &sessions()).await?;
    let (alpha, gamma, _) = chosen(&phil)?;
    let desk = desk(&phil, false)?;
    let (_, env) = phil.loaded(false)?;
    capture::sanitize::LocalKey::load_or_create(&capture::share::key_path(&env)?)?;
    let before = written(&phil);

    let draft = desk.draft(request(&[&alpha, &gamma], true)?).await?;
    desk.report(draft.id).await?;
    desk.mark(
        draft.id,
        vec![Mark {
            number: 1,
            choice: FlagChoice::Keep,
            learn: false,
        }],
    )
    .await?;

    assert_eq!(written(&phil), before, "a draft wrote under the home");
    model.close().await?;
    Ok(())
}

#[derive(Clone, Default)]
struct Drops(Arc<Mutex<Vec<StatusCode>>>);

async fn upload(
    axum::extract::State(drops): axum::extract::State<Drops>,
) -> axum::response::Response {
    let refused = drops.0.lock().pop();
    match refused {
        Some(status) => (status, "busy").into_response(),
        None => (
            StatusCode::CREATED,
            r#"{"id":"Qm9iQm9iQm9iQm9iQm9iQg","expires_at":"2026-11-17T12:00:00Z","bytes":512}"#,
        )
            .into_response(),
    }
}

fn sign_in(node: &Node, host: &str) -> TestResult {
    let path = node
        .home
        .path()
        .join(".local/state/rivethub/share-logins.json");
    std::fs::create_dir_all(path.parent().ok_or("no parent")?)?;
    std::fs::write(
        &path,
        format!(
            r#"{{"logins":{{"{host}":{{"access_token":"rhca_x","refresh_token":"rhcr_x","expires_at":"2099-01-01T00:00:00Z","scope":"drops:write"}}}}}}"#
        ),
    )?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn send(host: &str, sessions: usize, kept_flags: usize) -> SendRequest {
    SendRequest {
        server: host.to_owned().into(),
        signed: true,
        confirm: SendConfirm {
            sessions,
            kept_flags,
        },
    }
}

#[tokio::test]
async fn send_refuses_open_flags_and_wrong_counts() -> TestResult {
    let model = model().await?;
    let drops = Drops::default();
    let drop_box = testkit::stub_server(
        axum::Router::new()
            .route("/api/drops", axum::routing::post(upload))
            .with_state(drops.clone()),
    )
    .await?;
    let host = drop_box.addr.to_string();
    let phil = Node::new("phil", &format!("http://{}", model.addr), &sessions()).await?;
    sign_in(&phil, &host)?;
    let (alpha, gamma, _) = chosen(&phil)?;
    let desk = desk(&phil, false)?;
    let draft = desk.draft(request(&[&alpha, &gamma], true)?).await?;
    assert_eq!(draft.open_flags(), 1);

    let refused = desk.send(draft.id, send(&host, 2, 0)).await;
    assert!(
        matches!(refused, Err(DeskError::OpenFlags { open: 1 })),
        "{refused:?}"
    );
    let marked = desk
        .mark(
            draft.id,
            vec![Mark {
                number: 1,
                choice: FlagChoice::Keep,
                learn: false,
            }],
        )
        .await?;
    assert_eq!(marked.draft.kept_flags(), 1);
    for (sessions, kept) in [(1, 1), (2, 0)] {
        let refused = desk.send(draft.id, send(&host, sessions, kept)).await;
        assert!(
            matches!(refused, Err(DeskError::Counts { .. })),
            "{refused:?}"
        );
    }

    drops.0.lock().push(StatusCode::SERVICE_UNAVAILABLE);
    let refused = desk.send(draft.id, send(&host, 2, 1)).await;
    assert!(
        matches!(refused, Err(DeskError::DropBox { status: 503, .. })),
        "{refused:?}"
    );
    assert_eq!(
        desk.report(draft.id).await?.kept_flags(),
        1,
        "a failed upload keeps the draft"
    );

    let sent = desk.send(draft.id, send(&host, 2, 1)).await?;
    assert!(
        sent.code.contains("Qm9iQm9iQm9iQm9iQm9iQg#"),
        "a code names its drop"
    );
    assert!(sent.signed.is_some());
    assert!(matches!(
        desk.report(draft.id).await,
        Err(DeskError::UnknownDraft)
    ));
    model.close().await?;
    drop_box.close().await?;
    Ok(())
}

#[derive(Clone)]
struct Gated {
    arrived: Arc<tokio::sync::Notify>,
    gate: Arc<tokio::sync::Semaphore>,
    uploaded: Arc<AtomicUsize>,
}

async fn gated_upload(
    axum::extract::State(drops): axum::extract::State<Gated>,
) -> axum::response::Response {
    drops.arrived.notify_one();
    let Ok(permit) = drops.gate.acquire().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    permit.forget();
    drops.uploaded.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::CREATED,
        r#"{"id":"Qm9iQm9iQm9iQm9iQm9iQg","expires_at":"2026-11-17T12:00:00Z","bytes":512}"#,
    )
        .into_response()
}

async fn gated_drop_box(phil: &Node) -> TestResult<(Gated, testkit::Stub)> {
    let drops = Gated {
        arrived: Arc::default(),
        gate: Arc::new(tokio::sync::Semaphore::new(0)),
        uploaded: Arc::default(),
    };
    let drop_box = testkit::stub_server(
        axum::Router::new()
            .route("/api/drops", axum::routing::post(gated_upload))
            .with_state(drops.clone()),
    )
    .await?;
    sign_in(phil, &drop_box.addr.to_string())?;
    Ok((drops, drop_box))
}

#[tokio::test]
async fn a_draft_sent_twice_at_once_uploads_once() -> TestResult {
    let phil = Node::new("phil", "http://127.0.0.1:9", &sessions()).await?;
    let (alpha, _, _) = chosen(&phil)?;
    let (drops, drop_box) = gated_drop_box(&phil).await?;
    let host = drop_box.addr.to_string();
    let desk = desk(&phil, false)?;
    let draft = desk.draft(request(&[&alpha], false)?).await?;

    let (first, second, ()) = tokio::join!(
        desk.send(draft.id, send(&host, 1, 0)),
        desk.send(draft.id, send(&host, 1, 0)),
        async {
            drops.arrived.notified().await;
            drops.gate.add_permits(2);
        },
    );

    let codes = [&first, &second].iter().filter(|sent| sent.is_ok()).count();
    assert_eq!(codes, 1, "{first:?} {second:?}");
    assert!(
        [&first, &second]
            .iter()
            .any(|sent| matches!(sent, Err(DeskError::UnknownDraft))),
        "{first:?} {second:?}"
    );
    assert_eq!(drops.uploaded.load(Ordering::SeqCst), 1);
    drop_box.close().await?;
    Ok(())
}

#[tokio::test]
async fn nothing_uploads_after_a_discard_returns() -> TestResult {
    let phil = Node::new("phil", "http://127.0.0.1:9", &sessions()).await?;
    let (alpha, _, _) = chosen(&phil)?;
    let (drops, drop_box) = gated_drop_box(&phil).await?;
    let host = drop_box.addr.to_string();
    let desk = desk(&phil, false)?;
    let draft = desk.draft(request(&[&alpha], false)?).await?;
    let discarding = tokio::sync::Notify::new();

    let (sent, (discarded, uploaded_by_then), ()) = tokio::join!(
        desk.send(draft.id, send(&host, 1, 0)),
        async {
            drops.arrived.notified().await;
            discarding.notify_one();
            let discarded = desk.discard(draft.id).await;
            (discarded, drops.uploaded.load(Ordering::SeqCst))
        },
        async {
            discarding.notified().await;
            drops.gate.add_permits(1);
        },
    );

    assert_eq!(
        drops.uploaded.load(Ordering::SeqCst),
        uploaded_by_then,
        "an upload finished after discard returned: {sent:?} {discarded:?}"
    );
    assert!(sent.is_ok() != discarded.is_ok(), "{sent:?} {discarded:?}");
    let late = desk.send(draft.id, send(&host, 1, 0)).await;
    assert!(matches!(late, Err(DeskError::UnknownDraft)), "{late:?}");
    drop_box.close().await?;
    Ok(())
}

#[tokio::test]
async fn debug_never_shows_text_or_code() -> TestResult {
    let model = model().await?;
    let phil = Node::new("phil", &format!("http://{}", model.addr), &sessions()).await?;
    let (_, gamma, _) = chosen(&phil)?;
    let desk = desk(&phil, false)?;
    let draft = desk.draft(request(&[&gamma], true)?).await?;
    let flag = draft.flags.first().ok_or("one flag")?;
    assert!(
        flag.text.as_str().contains(RISKY),
        "the report body carries the text"
    );
    assert_eq!(flag.state, FlagState::Open);

    for shown in [
        format!("{desk:?}"),
        format!("{draft:?}"),
        format!("{flag:?}"),
        format!("{:?}", flag.text),
    ] {
        assert!(!shown.contains(RISKY), "{shown}");
    }
    let mark = Mark {
        number: flag.number,
        choice: FlagChoice::Map {
            term: RISKY.to_owned(),
            placeholder: "PLACEHOLDER_SHOWN".to_owned(),
        },
        learn: false,
    };
    let marked = desk.mark(draft.id, vec![mark.clone()]).await?;
    let mapped = marked.draft.flags.first().ok_or("one flag")?;
    assert!(matches!(mapped.state, FlagState::Map { .. }));
    for shown in [
        format!("{marked:?}"),
        format!("{mapped:?}"),
        format!("{:?}", mapped.state),
        format!("{mark:?}"),
        format!("{:?}", mark.choice),
    ] {
        assert!(!shown.contains(RISKY), "{shown}");
        assert!(!shown.contains("PLACEHOLDER_SHOWN"), "{shown}");
    }
    let sent = kernel::memory::share::desk::Sent {
        code: "rh1:127.0.0.1:9/Qm9iQm9iQm9iQm9iQm9iQg#secretkeypart".to_owned(),
        expires_at: Utc::now(),
        bytes: 1,
        signed: None,
    };
    assert!(!format!("{sent:?}").contains("secretkeypart"));
    model.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_failing_decision_model_never_echoes_into_the_error() -> TestResult {
    let model = testkit::stub_server(
        axum::Router::new()
            .route(
                "/v1/decide",
                axum::routing::post(|body: String| async move {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("cannot score {body}"),
                    )
                }),
            )
            .route(
                "/v1/decide/info",
                axum::routing::get(|| async { r#"{"model": "stub-model"}"# }),
            ),
    )
    .await?;
    let phil = Node::new("phil", &format!("http://{}", model.addr), &sessions()).await?;
    let (_, gamma, _) = chosen(&phil)?;
    let desk = desk(&phil, false)?;

    let refused = desk.draft(request(&[&gamma], true)?).await;

    let Err(DeskError::Failed(message)) = refused else {
        return Err(format!("{refused:?}").into());
    };
    assert!(message.contains("500"), "{message}");
    assert!(!message.contains(RISKY), "{message}");
    assert!(!message.contains("cannot score"), "{message}");
    model.close().await?;
    Ok(())
}

#[tokio::test]
async fn drafts_are_capped_and_expire_when_idle() -> TestResult {
    let phil = Node::new("phil", "http://127.0.0.1:9", &sessions()).await?;
    let (alpha, _, _) = chosen(&phil)?;
    let start: DateTime<Utc> = "2026-11-11T12:00:00Z".parse()?;
    let minutes = Arc::new(AtomicI64::new(0));
    let clock = {
        let minutes = Arc::clone(&minutes);
        Arc::new(move || start + chrono::TimeDelta::minutes(minutes.load(Ordering::SeqCst)))
    };
    let desk = desk(&phil, false)?.with_clock(clock);

    let mut held: Vec<DraftId> = Vec::new();
    for _ in 0..4 {
        held.push(desk.draft(request(&[&alpha], false)?).await?.id);
    }
    let full = desk.draft(request(&[&alpha], false)?).await;
    assert!(matches!(full, Err(DeskError::Full)), "{full:?}");

    minutes.store(29, Ordering::SeqCst);
    let kept = *held.first().ok_or("a draft")?;
    let touched = desk.report(kept).await?;
    assert_eq!(touched.expires_at, start + chrono::TimeDelta::minutes(59));
    minutes.store(31, Ordering::SeqCst);
    desk.sweep();
    for id in held.iter().skip(1) {
        assert!(matches!(
            desk.report(*id).await,
            Err(DeskError::UnknownDraft)
        ));
    }
    desk.report(kept).await?;
    desk.draft(request(&[&alpha], false)?).await?;
    desk.discard(kept).await?;
    assert!(matches!(
        desk.discard(kept).await,
        Err(DeskError::UnknownDraft)
    ));
    Ok(())
}

async fn counting_model() -> TestResult<(Arc<AtomicUsize>, testkit::Stub)> {
    let described = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&described);
    let model = testkit::stub_server(
        axum::Router::new()
            .route("/v1/decide", axum::routing::post(decide))
            .route(
                "/v1/decide/info",
                axum::routing::get(move || async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    r#"{"model": "stub-model"}"#
                }),
            ),
    )
    .await?;
    Ok((described, model))
}

#[tokio::test]
async fn concurrent_drafts_hold_no_more_than_the_cap() -> TestResult {
    let (described, model) = counting_model().await?;
    let phil = Node::new("phil", &format!("http://{}", model.addr), &sessions()).await?;
    let (alpha, _, _) = chosen(&phil)?;
    let desk = desk(&phil, false)?;
    let wanted = request(&[&alpha], true)?;

    let drafted = futures::future::join_all((0..8).map(|_| desk.draft(wanted.clone()))).await;

    let held = drafted.iter().filter(|draft| draft.is_ok()).count();
    let full = drafted
        .iter()
        .filter(|draft| matches!(draft, Err(DeskError::Full)))
        .count();
    assert_eq!((held, full), (4, 4), "{drafted:?}");
    assert_eq!(
        described.load(Ordering::SeqCst),
        4,
        "a refused draft still ran the decision pass"
    );
    assert!(format!("{desk:?}").contains("drafts: 4"), "{desk:?}");
    model.close().await?;
    Ok(())
}

#[tokio::test]
async fn the_byte_cap_refuses_before_sanitizing() -> TestResult {
    let (described, model) = counting_model().await?;
    let phil = Node::new("phil", &format!("http://{}", model.addr), &sessions()).await?;
    let (alpha, _, _) = chosen(&phil)?;
    let desk = desk(&phil, false)?.with_byte_cap(1);

    let refused = desk.draft(request(&[&alpha], true)?).await;

    assert!(matches!(refused, Err(DeskError::Full)), "{refused:?}");
    assert_eq!(described.load(Ordering::SeqCst), 0);
    assert!(format!("{desk:?}").contains("drafts: 0"), "{desk:?}");
    model.close().await?;
    Ok(())
}

#[tokio::test]
async fn find_matches_session_find() -> TestResult {
    let phil = Node::new("phil", "http://127.0.0.1:9", &sessions()).await?;
    let desk = desk(&phil, false)?;
    let (refused, text) = call(
        &phil.server(false)?,
        "session_find",
        r#"{"query":"bundle"}"#,
    )
    .await?;
    assert!(!refused, "{text}");

    let found = desk
        .find(FindQuery {
            query: "bundle".to_owned(),
            since: None,
            before: None,
            agent: None,
            tag: None,
            limit: None,
        })
        .await?;

    assert_eq!(found.sessions.len(), 2);
    let mut rest = text.as_str();
    for (index, session) in found.sessions.iter().enumerate() {
        let head = format!(
            "{}. {}  \"{}\"  {}",
            index + 1,
            session.id,
            session.title.as_deref().unwrap_or("untitled"),
            session.agent
        );
        let at = rest
            .find(&head)
            .ok_or_else(|| format!("{head} missing from {text}"))?;
        rest = &rest[at..];
        for excerpt in &session.excerpts {
            assert!(rest.contains(&format!("   > {excerpt}\n")), "{excerpt}");
        }
    }
    assert_eq!(
        text.lines().filter(|line| line.contains("  best ")).count(),
        found.sessions.len()
    );
    Ok(())
}

async fn echo_the_bearer(headers: axum::http::HeaderMap) -> axum::response::Response {
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("upstream saw {bearer}"),
    )
        .into_response()
}

#[tokio::test]
async fn a_share_server_echoing_the_token_never_reaches_the_desk_error() -> TestResult {
    const TEAM_TOKEN: &str = "rht_team_canary";
    let echo = testkit::stub_server(
        axum::Router::new()
            .route("/api/me", axum::routing::get(echo_the_bearer))
            .route("/api/drops", axum::routing::post(echo_the_bearer)),
    )
    .await?;
    let host = echo.addr.to_string();
    let phil = Node::new("phil", "http://127.0.0.1:9", &sessions()).await?;
    let (alpha, _, _) = chosen(&phil)?;
    let desk = desk(&phil, false)?;

    let signed_in = desk
        .login(kernel::memory::share::desk::LoginRequest::Team {
            origin: host.clone().into(),
            token: TEAM_TOKEN.to_owned(),
        })
        .await;
    sign_in(&phil, &host)?;
    let draft = desk.draft(request(&[&alpha], false)?).await?;
    let sent = desk.send(draft.id, send(&host, 1, 0)).await;

    for (step, refused) in [("login", signed_in.err()), ("send", sent.err())] {
        let refused = refused.ok_or("the echoing server must fail the step")?;
        assert!(
            matches!(refused, DeskError::DropBox { status: 500, .. }),
            "{step}: {refused:?}"
        );
        let shown = format!("{refused} {refused:?}");
        for secret in [TEAM_TOKEN, "rhca_x", "upstream saw"] {
            assert!(!shown.contains(secret), "{step}: {shown}");
        }
    }
    echo.close().await?;
    Ok(())
}

#[derive(Clone)]
struct Approver {
    gate: Arc<tokio::sync::Semaphore>,
    asked: Arc<tokio::sync::Notify>,
    device_gate: Arc<tokio::sync::Semaphore>,
    device_asked: Arc<tokio::sync::Notify>,
}

async fn oauth_metadata(headers: axum::http::HeaderMap) -> axum::response::Response {
    let host = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    (
        [("content-type", "application/json")],
        format!(
            r#"{{"device_authorization_endpoint":"http://{host}/oauth/device","token_endpoint":"http://{host}/oauth/token","revocation_endpoint":"http://{host}/oauth/revoke"}}"#
        ),
    )
        .into_response()
}

async fn oauth_device(
    axum::extract::State(approver): axum::extract::State<Approver>,
) -> axum::response::Response {
    approver.device_asked.notify_one();
    if let Ok(permit) = approver.device_gate.acquire().await {
        permit.forget();
    }
    (
        [("content-type", "application/json")],
        r#"{"device_code":"rhcd_x","user_code":"ABCD-EFGH","verification_uri":"http://example.invalid/device","verification_uri_complete":"http://example.invalid/device#user_code=ABCD-EFGH","expires_in":600,"interval":1}"#,
    )
        .into_response()
}

async fn oauth_token(
    axum::extract::State(approver): axum::extract::State<Approver>,
) -> axum::response::Response {
    approver.asked.notify_one();
    let Ok(permit) = approver.gate.acquire().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    permit.forget();
    (
        [("content-type", "application/json")],
        r#"{"access_token":"rhca_late","token_type":"Bearer","expires_in":900,"refresh_token":"rhcr_late","scope":"drops:write"}"#,
    )
        .into_response()
}

#[tokio::test]
async fn approving_a_login_after_logout_does_not_sign_the_desk_back_in() -> TestResult {
    let approver = Approver {
        gate: Arc::new(tokio::sync::Semaphore::new(0)),
        asked: Arc::default(),
        device_gate: Arc::new(tokio::sync::Semaphore::new(1)),
        device_asked: Arc::default(),
    };
    let share = testkit::stub_server(
        axum::Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(oauth_metadata),
            )
            .route("/oauth/device", axum::routing::post(oauth_device))
            .route("/oauth/token", axum::routing::post(oauth_token))
            .route(
                "/oauth/revoke",
                axum::routing::post(|| async { StatusCode::OK }),
            )
            .with_state(approver.clone()),
    )
    .await?;
    let host = share.addr.to_string();
    let phil = Node::new("phil", "http://127.0.0.1:9", &sessions()).await?;
    let desk = desk(&phil, false)?;

    let started = desk
        .login(kernel::memory::share::desk::LoginRequest::Cloud {
            origin: host.clone().into(),
        })
        .await?;
    let kernel::memory::share::desk::LoginStep::Started { login, .. } = started else {
        return Err(format!("{started:?}").into());
    };
    approver.asked.notified().await;
    desk.logout(host.clone().into()).await?;
    approver.gate.add_permits(1);

    let polled = desk.poll(login).await;
    assert!(
        matches!(polled, Err(DeskError::UnknownLogin)),
        "a logged-out login stayed pollable: {polled:?}"
    );
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            sleep,
            reason = "no signal says a cancelled login stayed quiet, so the test gives a late save time to show"
        )
    )]
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let servers = desk.servers().await?;
    assert!(
        servers.servers.iter().all(|server| !server.signed_in),
        "the approval signed the desk back in: {servers:?}"
    );
    share.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_login_still_starting_when_logout_runs_cannot_sign_the_desk_in() -> TestResult {
    let approver = Approver {
        gate: Arc::new(tokio::sync::Semaphore::new(1)),
        asked: Arc::default(),
        device_gate: Arc::new(tokio::sync::Semaphore::new(0)),
        device_asked: Arc::default(),
    };
    let share = testkit::stub_server(
        axum::Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(oauth_metadata),
            )
            .route("/oauth/device", axum::routing::post(oauth_device))
            .route("/oauth/token", axum::routing::post(oauth_token))
            .route(
                "/oauth/revoke",
                axum::routing::post(|| async { StatusCode::OK }),
            )
            .with_state(approver.clone()),
    )
    .await?;
    let host = share.addr.to_string();
    let phil = Node::new("phil", "http://127.0.0.1:9", &sessions()).await?;
    let desk = desk(&phil, false)?;

    let (started, logged_out) = tokio::join!(
        desk.login(kernel::memory::share::desk::LoginRequest::Cloud {
            origin: host.clone().into(),
        }),
        async {
            approver.device_asked.notified().await;
            let mut logout = std::pin::pin!(desk.logout(host.clone().into()));
            let early =
                tokio::time::timeout(std::time::Duration::from_millis(200), &mut logout).await;
            approver.device_gate.add_permits(1);
            match early {
                Ok(done) => done,
                Err(_) => logout.await,
            }
        },
    );
    logged_out?;
    let kernel::memory::share::desk::LoginStep::Started { login, .. } = started? else {
        return Err("the login did not start".into());
    };

    let polled = desk.poll(login).await;
    assert!(
        matches!(polled, Err(DeskError::UnknownLogin)),
        "a login that started during logout stayed pollable: {polled:?}"
    );
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            sleep,
            reason = "no signal says a cancelled login stayed quiet, so the test gives a late save time to show"
        )
    )]
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let servers = desk.servers().await?;
    assert!(
        servers.servers.iter().all(|server| !server.signed_in),
        "a login that started during logout signed the desk in: {servers:?}"
    );
    share.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_login_stalled_on_one_origin_does_not_delay_logout_on_another() -> TestResult {
    let approver = Approver {
        gate: Arc::new(tokio::sync::Semaphore::new(1)),
        asked: Arc::default(),
        device_gate: Arc::new(tokio::sync::Semaphore::new(0)),
        device_asked: Arc::default(),
    };
    let share = testkit::stub_server(
        axum::Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(oauth_metadata),
            )
            .route("/oauth/device", axum::routing::post(oauth_device))
            .route("/oauth/token", axum::routing::post(oauth_token))
            .with_state(approver.clone()),
    )
    .await?;
    let host = share.addr.to_string();
    let phil = Node::new("phil", "http://127.0.0.1:9", &sessions()).await?;
    let desk = desk(&phil, false)?;

    let (started, other_logout) = tokio::join!(
        desk.login(kernel::memory::share::desk::LoginRequest::Cloud {
            origin: host.clone().into(),
        }),
        async {
            approver.device_asked.notified().await;
            let other = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                desk.logout("127.0.0.2:9".to_owned().into()),
            )
            .await;
            approver.device_gate.add_permits(1);
            other
        },
    );

    let other_logout =
        other_logout.map_err(|_| "logout on another origin waited on the stalled login")?;
    other_logout?;
    started?;
    share.close().await?;
    Ok(())
}
