#![cfg(test)]

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::Uri;
use axum::response::IntoResponse as _;
use axum::response::Response;
use cli::memory::desk;
use cli::memory::desk::Approval;
use config::Environment;
use parking_lot::Mutex;
use tempfile::TempDir;

use crate::support;

const GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    content_type: String,
    authorization: String,
    params: BTreeMap<String, String>,
}

fn header(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

#[derive(Clone)]
struct Reply {
    status: StatusCode,
    body: String,
}

fn ok(body: &str) -> Reply {
    Reply {
        status: StatusCode::OK,
        body: body.to_owned(),
    }
}

fn oauth_error(code: &str) -> Reply {
    Reply {
        status: StatusCode::BAD_REQUEST,
        body: format!(r#"{{"error":"{code}"}}"#),
    }
}

#[derive(Default)]
struct Script {
    seen: Vec<Seen>,
    replies: BTreeMap<String, VecDeque<Reply>>,
    metadata: Option<String>,
    me: Option<Reply>,
}

#[derive(Clone, Default)]
struct Auth(Arc<Mutex<Script>>);

impl Auth {
    fn queue(&self, path: &str, replies: impl IntoIterator<Item = Reply>) {
        self.0
            .lock()
            .replies
            .entry(path.to_owned())
            .or_default()
            .extend(replies);
    }

    fn seen(&self) -> Vec<Seen> {
        self.0.lock().seen.clone()
    }

    fn paths(&self) -> Vec<String> {
        self.seen().into_iter().map(|seen| seen.path).collect()
    }
}

async fn post(State(auth): State<Auth>, uri: Uri, headers: HeaderMap, body: String) -> Response {
    let reply = {
        let mut script = auth.0.lock();
        script.seen.push(Seen {
            path: uri.path().to_owned(),
            content_type: header(&headers, "content-type"),
            authorization: header(&headers, "authorization"),
            params: serde_urlencoded::from_str(&body).unwrap_or_default(),
        });
        script
            .replies
            .get_mut(uri.path())
            .and_then(VecDeque::pop_front)
    };
    let Some(reply) = reply else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "unscripted").into_response();
    };
    (
        reply.status,
        [("content-type", "application/json; charset=utf-8")],
        reply.body,
    )
        .into_response()
}

async fn metadata(State(auth): State<Auth>) -> Response {
    let metadata = auth.0.lock().metadata.clone();
    match metadata {
        Some(body) => {
            (StatusCode::OK, [("content-type", "application/json")], body).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn me(State(auth): State<Auth>, uri: Uri, headers: HeaderMap) -> Response {
    let reply = {
        let mut script = auth.0.lock();
        script.seen.push(Seen {
            path: uri.path().to_owned(),
            content_type: header(&headers, "content-type"),
            authorization: header(&headers, "authorization"),
            params: BTreeMap::new(),
        });
        script.me.clone()
    };
    let reply = reply.unwrap_or_else(|| ok(r#"{"user":"phil"}"#));
    (
        reply.status,
        [("content-type", "application/json")],
        reply.body,
    )
        .into_response()
}

struct Served {
    auth: Auth,
    host: String,
    _stub: testkit::Stub,
}

async fn serve() -> Served {
    let auth = Auth::default();
    let app = Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            axum::routing::get(metadata),
        )
        .route("/api/me", axum::routing::get(me))
        .route("/v1/account", axum::routing::get(me))
        .route("/v1/updates/latest", axum::routing::get(me))
        .fallback(axum::routing::post(post))
        .with_state(auth.clone());
    let stub = testkit::stub_server(app).await.expect("fake server");
    let host = stub.addr.to_string();
    let base = format!("http://{host}/api/auth");
    auth.0.lock().metadata = Some(format!(
        r#"{{"device_authorization_endpoint":"{base}/device/code","token_endpoint":"{base}/oauth2/token","revocation_endpoint":"{base}/oauth2/revoke"}}"#
    ));
    Served {
        auth,
        host,
        _stub: stub,
    }
}

fn device(interval: u64) -> Reply {
    ok(&format!(
        r#"{{"device_code":"rhcd_x","user_code":"ABCD-EFGH","verification_uri":"http://example.invalid/device","verification_uri_complete":"http://example.invalid/device#user_code=ABCD-EFGH","expires_in":600,"interval":{interval}}}"#
    ))
}

fn issued(name: &str) -> Reply {
    ok(&format!(
        r#"{{"access_token":"rhca_{name}","token_type":"Bearer","expires_in":900,"refresh_token":"rhcr_{name}","scope":"drops:write"}}"#
    ))
}

struct Node {
    home: TempDir,
    env: Environment,
}

fn node() -> Node {
    let home = support::home();
    support::config(&home, "");
    let env = support::environment(&home).with_override(
        "XDG_STATE_HOME",
        home.path().join("state").display().to_string(),
    );
    Node { home, env }
}

impl Node {
    fn logins(&self) -> String {
        std::fs::read_to_string(self.home.path().join("state/rivethub/share-logins.json"))
            .unwrap_or_default()
    }
}

async fn signed_in(node: &Node, host: &str) -> Result<Approval, cli::Error> {
    let started = desk::start(&node.env, Some(host), "RivetHub test").await?;
    let mut pending = started.pending;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(approval) = pending.poll().await? {
            return Ok(approval);
        }
        assert!(tokio::time::Instant::now() < deadline, "login never ended");
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                sleep,
                reason = "this stands in for rivethub-desktop, which polls the desk on its own clock"
            )
        )]
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn discovered_endpoints_get_form_bodies_and_a_pending_login_signs_in() {
    let served = serve().await;
    let base = format!("http://{}", served.host);
    served.auth.0.lock().metadata = Some(format!(
        r#"{{"device_authorization_endpoint":"{base}/oauth/device","token_endpoint":"{base}/oauth/token","revocation_endpoint":"{base}/oauth/revoke"}}"#
    ));
    served.auth.queue("/oauth/device", [device(1)]);
    served.auth.queue(
        "/oauth/token",
        [oauth_error("authorization_pending"), issued("one")],
    );
    let node = node();

    let approval = signed_in(&node, &served.host).await.expect("login");

    assert_eq!(
        approval,
        Approval::SignedIn {
            scope: "drops:write".to_owned()
        }
    );
    let seen = served.auth.seen();
    assert_eq!(
        served.auth.paths(),
        ["/oauth/device", "/oauth/token", "/oauth/token"]
    );
    for request in &seen {
        assert_eq!(
            request.content_type, "application/x-www-form-urlencoded",
            "{request:?}"
        );
        assert_eq!(
            request.params.get("client_id").map(String::as_str),
            Some("rivethub-cli")
        );
    }
    let asked = &seen.first().expect("device request").params;
    assert_eq!(
        asked.get("scope").map(String::as_str),
        Some("drops:write offline_access")
    );
    assert!(
        asked
            .get("client_name")
            .is_some_and(|name| name.starts_with("RivetHub test on ")),
        "{asked:?}"
    );
    let polled = &seen.get(1).expect("token request").params;
    assert_eq!(polled.get("grant_type").map(String::as_str), Some(GRANT));
    assert_eq!(
        polled.get("device_code").map(String::as_str),
        Some("rhcd_x")
    );
    assert!(node.logins().contains(&served.host), "{}", node.logins());
}

#[tokio::test]
async fn a_server_without_metadata_offers_no_device_sign_in() {
    let served = serve().await;
    served.auth.0.lock().metadata = None;
    let node = node();

    let error = desk::start(&node.env, Some(&served.host), "RivetHub test")
        .await
        .expect_err("no metadata, no device flow");

    assert!(
        error.to_string().contains("offers no device sign-in"),
        "{error}"
    );
    assert_eq!(served.auth.paths(), Vec::<String>::new());
}

#[tokio::test]
async fn denied_and_expired_approvals_end_the_login() {
    for (code, wanted) in [
        ("access_denied", Approval::Denied),
        ("expired_token", Approval::Expired),
    ] {
        let served = serve().await;
        served.auth.queue("/api/auth/device/code", [device(1)]);
        served
            .auth
            .queue("/api/auth/oauth2/token", [oauth_error(code)]);
        let node = node();

        let approval = signed_in(&node, &served.host).await.expect("login");

        assert_eq!(approval, wanted, "{code}");
        assert!(!node.logins().contains(&served.host), "{code}");
    }
}

#[tokio::test]
async fn slow_down_adds_five_seconds_to_the_interval() {
    let served = serve().await;
    served.auth.queue("/api/auth/device/code", [device(1)]);
    served.auth.queue(
        "/api/auth/oauth2/token",
        [oauth_error("slow_down"), issued("one")],
    );
    let node = node();
    let started = desk::start(&node.env, Some(&served.host), "RivetHub test")
        .await
        .expect("start");
    let mut pending = started.pending;
    assert_eq!(pending.interval(), Duration::from_secs(1));

    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            sleep,
            reason = "the first token poll answers slow_down at once; the next one waits six seconds, so half a second lands between them"
        )
    )]
    tokio::time::sleep(Duration::from_millis(500)).await;
    let polled = pending.poll().await.expect("poll");

    assert_eq!(polled, None, "still waiting out the longer interval");
    assert_eq!(pending.interval(), Duration::from_secs(6));
    let approval = tokio::time::timeout(Duration::from_secs(30), pending.finish())
        .await
        .expect("login never ended")
        .expect("login");
    assert!(
        matches!(approval, Approval::SignedIn { .. }),
        "{approval:?}"
    );
}

#[tokio::test]
async fn a_team_login_is_checked_with_api_me() {
    let served = serve().await;
    let node = node();

    let user = desk::login_team(&node.env, &served.host, "rht_good".to_owned())
        .await
        .expect("team login");

    assert_eq!(user, "phil");
    let seen = served.auth.seen();
    assert_eq!(served.auth.paths(), ["/api/me"]);
    assert_eq!(
        seen.first().map(|seen| seen.authorization.as_str()),
        Some("Bearer rht_good")
    );
    assert!(node.logins().contains(&served.host));

    served.auth.0.lock().me = Some(Reply {
        status: StatusCode::UNAUTHORIZED,
        body: r#"{"error":"invalid_token"}"#.to_owned(),
    });
    let other = self::node();
    let refused = desk::login_team(&other.env, &served.host, "rht_bad".to_owned()).await;
    assert!(
        matches!(
            refused,
            Err(cli::Error::ShareServer(
                cli::share_server::ServerError::TokenRefused { status: 401, .. }
            ))
        ),
        "{refused:?}"
    );
    assert!(!other.logins().contains(&served.host));
}

#[tokio::test]
async fn the_servers_listing_names_the_token_store() {
    let node = node();

    let servers = desk::servers(&node.env).await.expect("servers");

    assert_eq!(
        servers.token_store,
        kernel::memory::share::desk::TokenStore::File
    );
    assert!(servers.logins_file.ends_with("share-logins.json"));
}

#[tokio::test]
async fn a_hub_that_disables_the_cloud_gives_the_desk_no_server_to_sign_in_to() {
    let node = node();
    support::config(&node.home, "cloud:\n  disabled: true\n");

    let refused = desk::start(&node.env, None, "RivetHub test")
        .await
        .expect_err("the policy refuses rivethub.cloud");
    let desk_error = desk::desk_error(refused);
    let kernel::memory::share::desk::DeskError::BadServer(message) = desk_error else {
        panic!("a policy refusal is a bad server, not {desk_error:?}");
    };
    assert!(
        message.contains("disabled by this hub's policy"),
        "{message}"
    );
    let servers = desk::servers(&node.env).await.expect("servers");
    assert!(servers.servers.is_empty(), "{:?}", servers.servers);
}

#[tokio::test]
async fn token_replies_never_reach_an_error() {
    let unparsable = ok(
        r#"{"access_token":"rhca_LEAKED","token_type":"Bearer","expires_in":"900","refresh_token":"rhcr_LEAKED","scope":"drops:write"}"#,
    );
    let failing = Reply {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        body: r#"{"access_token":"rhca_LEAKED","refresh_token":"rhcr_LEAKED"}"#.to_owned(),
    };
    for (path, reply) in [
        ("/api/auth/oauth2/token", unparsable),
        ("/api/auth/device/code", failing),
    ] {
        let served = serve().await;
        served.auth.queue("/api/auth/device/code", [device(1)]);
        served
            .auth
            .0
            .lock()
            .replies
            .insert(path.to_owned(), [reply].into());
        let node = node();

        let error = signed_in(&node, &served.host)
            .await
            .expect_err("the token reply is refused");

        let text = format!("{error} {error:?}");
        let desk = desk::desk_error(error);
        for shown in [text, format!("{desk} {desk:?}")] {
            for token in ["rhca_LEAKED", "rhcr_LEAKED"] {
                assert!(!shown.contains(token), "{path}: {shown}");
            }
        }
    }
}

#[tokio::test]
async fn a_verification_url_must_open_in_a_browser() {
    for (uri, opens) in [
        ("javascript:alert(1)", false),
        ("file:///etc/passwd", false),
        ("https://share.example.com/device#user_code=ABCD-EFGH", true),
        ("http://127.0.0.1:9/device#user_code=ABCD-EFGH", true),
    ] {
        let served = serve().await;
        served.auth.queue(
            "/api/auth/device/code",
            [ok(&format!(
                r#"{{"device_code":"rhcd_x","user_code":"ABCD-EFGH","verification_uri":"https://share.example.com/device","verification_uri_complete":"{uri}","expires_in":600,"interval":1}}"#
            ))],
        );
        let node = node();

        let started = desk::start(&node.env, Some(&served.host), "RivetHub test").await;

        match started {
            Ok(started) => {
                assert!(opens, "{uri} was offered");
                assert_eq!(started.verification_uri_complete, uri);
            }
            Err(error) => {
                assert!(!opens, "{uri}: {error}");
                assert!(error.to_string().contains("https"), "{error}");
            }
        }
    }
}

mod account;
