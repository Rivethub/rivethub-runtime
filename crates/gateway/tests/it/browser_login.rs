use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use axum::http::Request;
use axum::http::StatusCode;
use gateway::AuthPolicy;
use gateway::Gateway;
use gateway::local_owner::LocalOwner;
use gateway::tenancy::Tenancy;
use gateway::tenancy::UserId;
use kernel::memory::MemoryStore;
use parking_lot::Mutex;
use testkit::FakeMemoryStore;
use tower::ServiceExt as _;
use wire::gateway::GatewayError;
use wire::gateway::auth::LoginCodeResponse;
use wire::gateway::auth::REAUTH_REQUIRED;
use wire::gateway::auth::RotateResponse;
use wire::gateway::auth::Secret;
use wire::gateway::auth::SessionRequest;
use wire::gateway::auth::SessionResponse;

use crate::common;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const LOCAL: &str = "127.0.0.1:41000";
const HOST: &str = "127.0.0.1:5174";
const COOKIE: &str = "rh_session";
const TLS_COOKIE: &str = "__Host-rh_session";

struct Rig {
    app: Router,
    owner: Arc<LocalOwner>,
}

fn policy() -> AuthPolicy {
    AuthPolicy {
        attempts_per_minute: NonZeroU32::MAX,
        ..AuthPolicy::default()
    }
}

fn rig_with(policy: AuthPolicy, secure: bool) -> TestResult<Rig> {
    let handle: Arc<dyn MemoryStore> = Arc::new(FakeMemoryStore::default());
    let owner = Arc::new(LocalOwner::unpersisted()?);
    let user = UserId::new(testkit::OWNER.to_owned())?;
    let gateway = Arc::new(
        Gateway::new(handle, common::settings())
            .with_local_owner(Arc::clone(&owner))
            .with_tenancy(Tenancy::local(user))
            .with_auth_policy(policy),
    );
    Ok(Rig {
        app: gateway.router(secure),
        owner,
    })
}

fn rig() -> TestResult<Rig> {
    rig_with(policy(), false)
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    fn json<T: serde::de::DeserializeOwned>(&self) -> TestResult<T> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn set_cookie(&self) -> Option<String> {
        self.headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with(COOKIE))
            .map(str::to_owned)
    }
}

struct Call<'a> {
    rig: &'a Rig,
    method: &'static str,
    path: String,
    peer: &'static str,
    headers: Vec<(String, String)>,
    body: Option<String>,
}

impl Rig {
    fn call(&self, method: &'static str, path: &str) -> Call<'_> {
        Call {
            rig: self,
            method,
            path: path.to_owned(),
            peer: LOCAL,
            headers: Vec::new(),
            body: None,
        }
    }

    fn token(&self) -> String {
        self.owner.current().expose().to_owned()
    }
}

impl Call<'_> {
    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    fn peer(mut self, peer: &'static str) -> Self {
        self.peer = peer;
        self
    }

    fn json(mut self, body: &impl serde::Serialize) -> TestResult<Self> {
        self.body = Some(serde_json::to_string(body)?);
        Ok(self.header("content-type", "application/json"))
    }

    fn owner(self) -> Self {
        let value = format!("Bearer {}", self.rig.token());
        self.header("authorization", &value)
    }

    fn browser(self, browser: &Browser) -> Self {
        self.header("cookie", &format!("{COOKIE}={}", browser.session))
            .header("x-rh-csrf", &browser.csrf)
    }

    async fn send(self) -> TestResult<Reply> {
        let mut builder = Request::builder().method(self.method).uri(&self.path);
        if !self.headers.iter().any(|(name, _)| name == "host") {
            builder = builder.header("host", HOST);
        }
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        let mut request = builder.body(Body::from(self.body.unwrap_or_default()))?;
        request
            .extensions_mut()
            .insert(ConnectInfo(self.peer.parse::<std::net::SocketAddr>()?));
        let response = self.rig.app.clone().oneshot(request).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await?
            .to_vec();
        Ok(Reply {
            status,
            headers,
            body,
        })
    }
}

struct Browser {
    session: String,
    csrf: String,
}

async fn mint(rig: &Rig) -> TestResult<String> {
    let reply = rig
        .call("POST", "/api/auth/login-codes")
        .owner()
        .send()
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body: LoginCodeResponse = reply.json()?;
    Ok(body.code.expose().to_owned())
}

fn session_request(code: &str) -> SessionRequest {
    SessionRequest {
        code: Secret::new(code.to_owned()),
    }
}

async fn exchange(rig: &Rig, code: &str) -> TestResult<Reply> {
    rig.call("POST", "/api/auth/session")
        .json(&session_request(code))?
        .send()
        .await
}

fn browser_of(reply: &Reply) -> TestResult<Browser> {
    let cookie = reply.set_cookie().ok_or("no session cookie was set")?;
    let session = cookie
        .strip_prefix(&format!("{COOKIE}="))
        .and_then(|rest| rest.split(';').next())
        .ok_or("malformed cookie")?
        .to_owned();
    let body: SessionResponse = reply.json()?;
    Ok(Browser {
        session,
        csrf: body.csrf.expose().to_owned(),
    })
}

async fn login(rig: &Rig) -> TestResult<Browser> {
    let code = mint(rig).await?;
    let reply = exchange(rig, &code).await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    browser_of(&reply)
}

#[tokio::test]
async fn a_login_code_works_once() -> TestResult {
    let rig = rig()?;
    let code = mint(&rig).await?;
    assert_eq!(code.len(), 43);
    let first = exchange(&rig, &code).await?;
    assert_eq!(first.status, StatusCode::OK);
    assert!(first.set_cookie().is_some());
    let again = exchange(&rig, &code).await?;
    assert_eq!(again.status, StatusCode::UNAUTHORIZED);
    assert!(again.set_cookie().is_none());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_login_code_expires_after_sixty_seconds() -> TestResult {
    let rig = rig()?;
    let live = mint(&rig).await?;
    let stale = mint(&rig).await?;
    tokio::time::advance(Duration::from_secs(59)).await;
    assert_eq!(exchange(&rig, &live).await?.status, StatusCode::OK);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(
        exchange(&rig, &stale).await?.status,
        StatusCode::UNAUTHORIZED
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn the_seventeenth_live_code_is_refused_until_one_expires() -> TestResult {
    let rig = rig()?;
    for _ in 0..16 {
        mint(&rig).await?;
    }
    let refused = rig
        .call("POST", "/api/auth/login-codes")
        .owner()
        .send()
        .await?;
    assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(refused.header("retry-after").is_some());
    tokio::time::advance(Duration::from_secs(61)).await;
    mint(&rig).await?;
    Ok(())
}

#[tokio::test]
async fn only_the_owner_token_mints_a_code() -> TestResult {
    let rig = rig()?;
    let none = rig.call("POST", "/api/auth/login-codes").send().await?;
    assert_eq!(none.status, StatusCode::UNAUTHORIZED);
    let browser = login(&rig).await?;
    let by_session = rig
        .call("POST", "/api/auth/login-codes")
        .browser(&browser)
        .send()
        .await?;
    assert_eq!(by_session.status, StatusCode::UNAUTHORIZED);
    let forwarded = rig
        .call("POST", "/api/auth/login-codes")
        .owner()
        .header("x-forwarded-for", "203.0.113.9")
        .send()
        .await?;
    assert_eq!(forwarded.status, StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn an_exchange_is_refused_behind_a_forwarder_and_keeps_the_code() -> TestResult {
    let rig = rig()?;
    let code = mint(&rig).await?;
    for headers in [
        vec![("host", "gateway.example.test")],
        vec![("x-forwarded-for", "203.0.113.9")],
        vec![("forwarded", "for=203.0.113.9")],
    ] {
        let mut call = rig
            .call("POST", "/api/auth/session")
            .json(&session_request(&code))?;
        for (name, value) in headers {
            call = call.header(name, value);
        }
        let reply = call.send().await?;
        assert!(
            matches!(
                reply.status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ),
            "{}",
            reply.status
        );
        assert!(reply.set_cookie().is_none());
    }
    let from_afar = rig
        .call("POST", "/api/auth/session")
        .peer("192.0.2.7:41000")
        .json(&session_request(&code))?
        .send()
        .await?;
    assert_eq!(from_afar.status, StatusCode::UNAUTHORIZED);
    assert_eq!(exchange(&rig, &code).await?.status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn a_failed_exchange_burns_no_live_code() -> TestResult {
    let rig = rig()?;
    let code = mint(&rig).await?;
    let wrong = exchange(&rig, "not-the-code").await?;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(exchange(&rig, &code).await?.status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn a_session_needs_the_right_csrf_header_on_reads_too() -> TestResult {
    let rig = rig()?;
    let browser = login(&rig).await?;
    let cookie = format!("{COOKIE}={}", browser.session);
    let bare = rig
        .call("GET", "/api/memory/stats")
        .header("cookie", &cookie)
        .send()
        .await?;
    assert_eq!(bare.status, StatusCode::UNAUTHORIZED);
    let wrong = rig
        .call("GET", "/api/memory/stats")
        .header("cookie", &cookie)
        .header("x-rh-csrf", "wrong")
        .send()
        .await?;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let right = rig
        .call("GET", "/api/memory/stats")
        .browser(&browser)
        .send()
        .await?;
    assert_eq!(right.status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn an_owner_browser_session_may_record_a_receipt() -> TestResult {
    let rig = rig()?;
    let browser = login(&rig).await?;
    let mut settings = wire::JsonObject::default();
    settings.insert(
        "receipt",
        wire::capture::Receipt {
            bundle_id: "rhb1:t".into(),
            received_at: "2026-11-09T00:00:00Z".parse()?,
            verification: wire::capture::ReceiptVerification::Verified,
            key_fingerprint: Some("ed25519:abcd".into()),
            checks: None,
        },
    )?;
    let body = wire::capture::CaptureBatchRequest {
        session_key: "import:[\"node\",\"conversation\"]".into(),
        agent: "rivet".into(),
        channel: None,
        title: None,
        settings: Some(settings),
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![wire::capture::CaptureMessage {
            event_id: "e1".into(),
            role: wire::message::Role::User,
            content: "hello".into(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: None,
        }],
    };
    let sent = rig
        .call("POST", "/api/capture")
        .browser(&browser)
        .json(&body)?
        .send()
        .await?;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.text());
    Ok(())
}

#[tokio::test]
async fn a_session_is_refused_behind_a_forwarder_or_on_a_foreign_host() -> TestResult {
    let rig = rig()?;
    let browser = login(&rig).await?;
    for (name, value) in [
        ("x-forwarded-for", "203.0.113.9"),
        ("forwarded", "for=203.0.113.9"),
        ("x-real-ip", "203.0.113.9"),
        ("host", "gateway.example.test"),
    ] {
        let reply = rig
            .call("GET", "/api/memory/stats")
            .browser(&browser)
            .header(name, value)
            .send()
            .await?;
        assert!(
            matches!(
                reply.status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ),
            "{name}: {}",
            reply.status
        );
    }
    Ok(())
}

#[tokio::test]
async fn the_wiki_page_accepts_the_cookie_alone() -> TestResult {
    let rig = rig()?;
    let browser = login(&rig).await?;
    let cookie = format!("{COOKIE}={}", browser.session);
    for path in ["/wiki", "/wiki/"] {
        let reply = rig
            .call("GET", path)
            .header("cookie", &cookie)
            .send()
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
    }
    let api = rig
        .call("GET", "/api/wiki")
        .header("cookie", &cookie)
        .send()
        .await?;
    assert_eq!(api.status, StatusCode::UNAUTHORIZED);
    let post = rig
        .call("POST", "/wiki")
        .header("cookie", &cookie)
        .send()
        .await?;
    assert_eq!(post.status, StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn logout_invalidates_the_session() -> TestResult {
    let rig = rig()?;
    let browser = login(&rig).await?;
    let without = rig
        .call("POST", "/api/auth/logout")
        .header("cookie", &format!("{COOKIE}={}", browser.session))
        .send()
        .await?;
    assert_eq!(without.status, StatusCode::UNAUTHORIZED);
    let out = rig
        .call("POST", "/api/auth/logout")
        .browser(&browser)
        .send()
        .await?;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    let after = rig
        .call("GET", "/api/memory/stats")
        .browser(&browser)
        .send()
        .await?;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn rotation_invalidates_the_old_token_every_session_and_every_code() -> TestResult {
    let rig = rig()?;
    let browser = login(&rig).await?;
    let pending = mint(&rig).await?;
    let old = rig.token();
    let rotated = rig.call("POST", "/api/auth/rotate").owner().send().await?;
    assert_eq!(rotated.status, StatusCode::OK, "{}", rotated.text());
    assert!(rotated.json::<RotateResponse>()?.rotated);
    assert_ne!(rig.token(), old);
    let by_old = rig
        .call("GET", "/api/memory/stats")
        .header("authorization", &format!("Bearer {old}"))
        .send()
        .await?;
    assert_eq!(by_old.status, StatusCode::UNAUTHORIZED);
    let by_new = rig.call("GET", "/api/memory/stats").owner().send().await?;
    assert_eq!(by_new.status, StatusCode::OK);
    let by_session = rig
        .call("GET", "/api/memory/stats")
        .browser(&browser)
        .send()
        .await?;
    assert_eq!(by_session.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        exchange(&rig, &pending).await?.status,
        StatusCode::UNAUTHORIZED
    );
    Ok(())
}

#[tokio::test]
async fn a_session_idle_past_its_limit_is_refused() -> TestResult {
    let rig = rig_with(
        AuthPolicy {
            idle: Duration::from_secs(1),
            ..policy()
        },
        false,
    )?;
    let browser = login(&rig).await?;
    let probe = || rig.call("GET", "/api/memory/stats").browser(&browser);
    assert_eq!(probe().send().await?.status, StatusCode::OK);
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            sleep,
            reason = "tower-sessions expires on inactivity against the wall clock, which a paused tokio clock does not move"
        )
    )]
    tokio::time::sleep(Duration::from_millis(2100)).await;
    assert_eq!(probe().send().await?.status, StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn a_session_past_its_absolute_limit_is_refused_however_active() -> TestResult {
    let rig = rig_with(
        AuthPolicy {
            absolute: Duration::ZERO,
            ..policy()
        },
        false,
    )?;
    let browser = login(&rig).await?;
    let reply = rig
        .call("GET", "/api/memory/stats")
        .browser(&browser)
        .send()
        .await?;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    Ok(())
}

fn cookie_attributes(cookie: &str) -> Vec<String> {
    cookie
        .split(';')
        .skip(1)
        .map(|part| part.trim().to_ascii_lowercase())
        .collect()
}

#[tokio::test]
async fn plain_http_loopback_sets_an_unprefixed_cookie_without_secure() -> TestResult {
    let rig = rig_with(policy(), false)?;
    let code = mint(&rig).await?;
    let reply = exchange(&rig, &code).await?;
    let cookie = reply.set_cookie().ok_or("no cookie")?;
    assert!(cookie.starts_with("rh_session="), "{cookie}");
    assert!(!cookie.contains("__Host-"), "{cookie}");
    let attributes = cookie_attributes(&cookie);
    for wanted in ["httponly", "samesite=strict", "path=/"] {
        assert!(
            attributes.iter().any(|a| a == wanted),
            "{wanted} in {cookie}"
        );
    }
    assert!(!attributes.iter().any(|a| a == "secure"), "{cookie}");
    assert!(
        !attributes.iter().any(|a| a.starts_with("domain")),
        "{cookie}"
    );
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    Ok(())
}

#[tokio::test]
async fn under_tls_the_cookie_is_host_prefixed_and_secure() -> TestResult {
    let rig = rig_with(policy(), true)?;
    let code = mint(&rig).await?;
    let reply = exchange(&rig, &code).await?;
    let cookie = reply
        .headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(TLS_COOKIE))
        .ok_or("no __Host- cookie")?
        .to_owned();
    let attributes = cookie_attributes(&cookie);
    for wanted in ["secure", "httponly", "samesite=strict", "path=/"] {
        assert!(
            attributes.iter().any(|a| a == wanted),
            "{wanted} in {cookie}"
        );
    }
    assert!(
        !attributes.iter().any(|a| a.starts_with("domain")),
        "{cookie}"
    );
    Ok(())
}

#[tokio::test]
async fn the_plain_cookie_is_not_honored_without_the_csrf_header() -> TestResult {
    let rig = rig_with(policy(), false)?;
    let browser = login(&rig).await?;
    let reply = rig
        .call("GET", "/api/memory/stats")
        .header("cookie", &format!("{COOKIE}={}", browser.session))
        .send()
        .await?;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn the_cookie_round_trips_from_127_0_0_1_and_localhost_origins() -> TestResult {
    let rig = rig()?;
    for authority in ["127.0.0.1:5174", "localhost:5174"] {
        let origin = format!("http://{authority}");
        let code = mint(&rig).await?;
        let reply = rig
            .call("POST", "/api/auth/session")
            .header("host", authority)
            .header("origin", &origin)
            .json(&session_request(&code))?
            .send()
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{authority}");
        let browser = browser_of(&reply)?;
        let read = rig
            .call("GET", "/api/memory/stats")
            .header("host", authority)
            .header("origin", &origin)
            .browser(&browser)
            .send()
            .await?;
        assert_eq!(read.status, StatusCode::OK, "{authority}");
    }
    Ok(())
}

#[tokio::test]
async fn an_exchange_cycles_the_session_id() -> TestResult {
    let rig = rig()?;
    let planted = rig
        .call("POST", "/api/auth/session")
        .header("cookie", &format!("{COOKIE}=planted-by-an-attacker"))
        .json(&session_request(&mint(&rig).await?))?
        .send()
        .await?;
    let adopted = browser_of(&planted)?;
    assert_ne!(adopted.session, "planted-by-an-attacker");

    let first = login(&rig).await?;
    let second = rig
        .call("POST", "/api/auth/session")
        .header("cookie", &format!("{COOKIE}={}", first.session))
        .json(&session_request(&mint(&rig).await?))?
        .send()
        .await?;
    let second = browser_of(&second)?;
    assert_ne!(second.session, first.session);
    let by_old = rig
        .call("GET", "/api/memory/stats")
        .browser(&first)
        .send()
        .await?;
    assert_eq!(by_old.status, StatusCode::UNAUTHORIZED);
    let by_new = rig
        .call("GET", "/api/memory/stats")
        .browser(&second)
        .send()
        .await?;
    assert_eq!(by_new.status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn failed_exchanges_are_rate_limited_but_a_valid_code_and_a_valid_mint_still_pass()
-> TestResult {
    let rig = rig_with(
        AuthPolicy {
            attempts_per_minute: NonZeroU32::new(3).ok_or("three")?,
            ..AuthPolicy::default()
        },
        false,
    )?;
    let code = mint(&rig).await?;
    for _ in 0..3 {
        assert_eq!(
            exchange(&rig, "wrong").await?.status,
            StatusCode::UNAUTHORIZED
        );
    }
    let limited = exchange(&rig, "wrong").await?;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    let wait: u64 = limited
        .header("retry-after")
        .ok_or("no Retry-After")?
        .parse()?;
    assert!((1..=60).contains(&wait), "{wait}");
    let anonymous_mint = rig
        .call("POST", "/api/auth/login-codes")
        .header("authorization", "Bearer wrong")
        .send()
        .await?;
    assert_eq!(anonymous_mint.status, StatusCode::UNAUTHORIZED);
    let owner_mint = rig
        .call("POST", "/api/auth/login-codes")
        .owner()
        .send()
        .await?;
    assert_eq!(owner_mint.status, StatusCode::OK);
    assert_eq!(exchange(&rig, &code).await?.status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn an_anonymous_flood_cannot_lock_the_owner_out() -> TestResult {
    let rig = rig_with(AuthPolicy::default(), false)?;
    for _ in 0..30 {
        exchange(&rig, "wrong").await?;
    }
    let code = mint(&rig).await?;
    assert_eq!(exchange(&rig, &code).await?.status, StatusCode::OK);
    Ok(())
}

async fn attempt(rig: &Rig, peer: &'static str) -> TestResult<Reply> {
    rig.call("POST", "/api/auth/session")
        .peer(peer)
        .json(&session_request("wrong"))?
        .send()
        .await
}

#[tokio::test]
async fn a_global_cap_limits_attempts_across_peers() -> TestResult {
    let rig = rig_with(
        AuthPolicy {
            attempts_per_minute: NonZeroU32::new(2).ok_or("two")?,
            ..AuthPolicy::default()
        },
        false,
    )?;
    for peer in ["127.0.0.1:41000", "[::1]:41000"] {
        for _ in 0..2 {
            assert_eq!(attempt(&rig, peer).await?.status, StatusCode::UNAUTHORIZED);
        }
    }
    let third = attempt(&rig, "[::ffff:127.0.0.1]:41000").await?;
    assert_eq!(third.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(third.header("retry-after").is_some());
    Ok(())
}

#[tokio::test]
async fn a_cross_origin_unlisted_or_null_exchange_is_refused() -> TestResult {
    let rig = rig()?;
    let code = mint(&rig).await?;
    for origin in ["null", "https://evil.example", "http://127.0.0.1:9999"] {
        let reply = rig
            .call("POST", "/api/auth/session")
            .header("origin", origin)
            .json(&session_request(&code))?
            .send()
            .await?;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{origin}");
    }
    assert_eq!(exchange(&rig, &code).await?.status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn the_login_page_loads_without_a_credential_with_a_hashed_script() -> TestResult {
    use base64::Engine as _;
    use sha2::Digest as _;
    let rig = rig()?;
    let reply = rig.call("GET", "/login").send().await?;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    let html = reply.text();
    assert!(html.contains("history.replaceState"));
    assert!(html.contains("location.hash"));
    assert!(html.contains("/api/auth/session"));
    let script = html
        .split_once("<script>")
        .and_then(|(_, rest)| rest.split_once("</script>"))
        .map(|(script, _)| script)
        .ok_or("no inline script")?;
    let hash = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(script));
    let csp = reply
        .header("content-security-policy")
        .ok_or("no CSP")?
        .to_owned();
    assert!(
        csp.contains(&format!("script-src 'sha256-{hash}'")),
        "{csp}"
    );
    assert!(csp.contains("default-src 'self'"));
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(!csp.contains("unsafe-inline"));
    assert_eq!(html.matches("<script").count(), 1);
    let head = rig.call("HEAD", "/login").send().await?;
    assert_eq!(head.status, StatusCode::OK);
    Ok(())
}

fn assert_hardened(reply: &Reply, what: &str) {
    assert_eq!(reply.header("x-frame-options"), Some("DENY"), "{what}");
    assert_eq!(
        reply.header("referrer-policy"),
        Some("no-referrer"),
        "{what}"
    );
    assert_eq!(
        reply.header("x-content-type-options"),
        Some("nosniff"),
        "{what}"
    );
    let csp = reply.header("content-security-policy").unwrap_or_default();
    assert!(csp.contains("frame-ancestors 'none'"), "{what}: {csp}");
}

const SHELL_POLICY: &str = "default-src 'self'; script-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

fn assert_shell_policy(reply: &Reply, what: &str) {
    assert_hardened(reply, what);
    assert_eq!(
        reply.header("content-security-policy"),
        Some(SHELL_POLICY),
        "{what}"
    );
}

#[tokio::test]
async fn pages_and_the_api_carry_the_security_headers() -> TestResult {
    let dir = tempfile::tempdir()?;
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><title>app</title>",
    )?;
    let mut settings = common::settings();
    settings.static_dir = Some(dir.path().to_path_buf());
    let handle: Arc<dyn MemoryStore> = Arc::new(FakeMemoryStore::default());
    let owner = Arc::new(LocalOwner::unpersisted()?);
    let gateway = Arc::new(
        Gateway::new(handle, settings)
            .with_local_owner(Arc::clone(&owner))
            .with_tenancy(Tenancy::local(UserId::new(testkit::OWNER.to_owned())?)),
    );
    let rig = Rig {
        app: gateway.router(false),
        owner,
    };
    let shell = rig.call("GET", "/").send().await?;
    assert_eq!(shell.status, StatusCode::OK);
    assert_shell_policy(&shell, "static shell");
    assert!(
        shell
            .header("content-security-policy")
            .unwrap_or_default()
            .contains("default-src 'self'")
    );
    let spa = rig.call("GET", "/somewhere").send().await?;
    assert_shell_policy(&spa, "spa route");
    let asset = rig.call("GET", "/index.html").send().await?;
    assert_shell_policy(&asset, "static file");
    let page = rig.call("GET", "/login").send().await?;
    assert_hardened(&page, "login");
    let wiki = rig.call("GET", "/wiki").owner().send().await?;
    assert_hardened(&wiki, "wiki");
    let refused = rig.call("GET", "/api/memory/stats").send().await?;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_shell_policy(&refused, "401");
    assert_eq!(refused.header("cache-control"), Some("no-store"));
    let api = rig.call("GET", "/api/memory/stats").owner().send().await?;
    assert_shell_policy(&api, "api");
    assert_eq!(api.header("cache-control"), Some("no-store"));
    let health = rig.call("GET", "/healthz").send().await?;
    assert_shell_policy(&health, "healthz");
    Ok(())
}

#[tokio::test]
async fn cors_echoes_a_listed_origin_with_credentials_and_nothing_else() -> TestResult {
    let rig = rig()?;
    let listed = "https://hub.example.test";
    let echoed = rig
        .call("GET", "/api/memory/stats")
        .owner()
        .header("origin", listed)
        .send()
        .await?;
    assert_eq!(echoed.status, StatusCode::OK);
    assert_eq!(echoed.header("access-control-allow-origin"), Some(listed));
    assert_eq!(
        echoed.header("access-control-allow-credentials"),
        Some("true")
    );
    let unlisted = rig
        .call("GET", "/api/memory/stats")
        .owner()
        .header("origin", "https://evil.example")
        .send()
        .await?;
    assert!(unlisted.header("access-control-allow-origin").is_none());
    let null = rig
        .call("GET", "/api/memory/stats")
        .owner()
        .header("origin", "null")
        .send()
        .await?;
    assert!(null.header("access-control-allow-origin").is_none());
    let same_origin = rig.call("GET", "/api/memory/stats").owner().send().await?;
    assert!(same_origin.header("access-control-allow-origin").is_none());
    Ok(())
}

#[tokio::test]
async fn cors_preflight_is_answered_for_a_listed_origin_only() -> TestResult {
    let rig = rig()?;
    let preflight = |origin: &'static str| {
        rig.call("OPTIONS", "/api/memory/stats")
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                "authorization, content-type, x-rh-csrf",
            )
    };
    let listed = preflight("https://hub.example.test").send().await?;
    assert!(listed.status.is_success(), "{}", listed.status);
    assert_eq!(
        listed.header("access-control-allow-origin"),
        Some("https://hub.example.test")
    );
    assert_eq!(
        listed.header("access-control-allow-credentials"),
        Some("true")
    );
    let headers = listed
        .header("access-control-allow-headers")
        .unwrap_or_default()
        .to_ascii_lowercase();
    for name in ["authorization", "content-type", "x-rh-csrf"] {
        assert!(headers.contains(name), "{headers}");
    }
    let methods = listed
        .header("access-control-allow-methods")
        .unwrap_or_default();
    for method in ["GET", "POST", "PATCH", "DELETE"] {
        assert!(methods.contains(method), "{methods}");
    }
    for origin in ["https://evil.example", "null"] {
        let reply = preflight(origin).send().await?;
        assert!(
            reply.header("access-control-allow-origin").is_none(),
            "{origin}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn cors_trusts_exact_normalized_origins_only_and_never_a_wildcard() -> TestResult {
    let rig = rig()?;
    let echoed = ["https://hub.example.test", "app://bundle"];
    let refused = [
        "null",
        "*",
        "",
        "https://evil.example",
        "https://hub.example.test/",
        "https://hub.example.test/path",
        "https://HUB.example.test",
        "https://hub.example.test:443",
        "http://hub.example.test",
        "https://hub.example.test.evil.example",
        "https://evil.example, https://hub.example.test",
        "http://127.0.0.1:5174",
        "http://localhost:5174",
    ];
    for origin in echoed.iter().chain(refused.iter()) {
        let listed = echoed.contains(origin);
        let preflight = rig
            .call("OPTIONS", "/api/memory/stats")
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "authorization, x-rh-csrf")
            .send()
            .await?;
        let simple = rig
            .call("GET", "/api/memory/stats")
            .owner()
            .header("origin", origin)
            .send()
            .await?;
        for (kind, reply) in [("preflight", &preflight), ("request", &simple)] {
            let allowed = reply.header("access-control-allow-origin");
            let credentials = reply.header("access-control-allow-credentials");
            if listed {
                assert_eq!(allowed, Some(*origin), "{kind} {origin}");
                assert_eq!(credentials, Some("true"), "{kind} {origin}");
            } else {
                assert_eq!(allowed, None, "{kind} {origin:?}");
                assert_eq!(credentials, None, "{kind} {origin:?}");
            }
            for (name, value) in &reply.headers {
                if name.as_str().starts_with("access-control-") {
                    assert!(
                        !value.to_str().unwrap_or_default().contains('*'),
                        "{kind} {origin:?}: {name} is a wildcard"
                    );
                }
            }
        }
    }
    let bare = rig.call("GET", "/healthz").send().await?;
    assert!(bare.header("access-control-allow-origin").is_none());
    assert!(bare.header("access-control-allow-credentials").is_none());
    Ok(())
}

async fn reauth_rig(fresh: Duration) -> TestResult<(Rig, Browser)> {
    let rig = rig_with(AuthPolicy { fresh, ..policy() }, false)?;
    let browser = login(&rig).await?;
    Ok((rig, browser))
}

const STEP_UP: [(&str, &str); 10] = [
    ("POST", "/api/auth/rotate"),
    ("PATCH", "/api/account"),
    ("DELETE", "/api/account"),
    ("POST", "/api/account/signin"),
    ("POST", "/api/account/signout"),
    ("GET", "/api/memory/export"),
    (
        "POST",
        "/api/share/drafts/0b7a7c62-3c43-4a1e-8d57-6d7a4f2a9e01/send",
    ),
    ("POST", "/api/share/servers/login"),
    ("POST", "/api/share/servers/logout"),
    ("POST", "/api/share/trust"),
];

fn reauth_body(reply: &Reply) -> TestResult<bool> {
    if reply.status != StatusCode::UNAUTHORIZED {
        return Ok(false);
    }
    let body: GatewayError = reply.json()?;
    Ok(body == GatewayError::new(REAUTH_REQUIRED))
}

#[tokio::test]
async fn a_stale_session_must_reauthenticate_on_every_sensitive_route() -> TestResult {
    let (rig, browser) = reauth_rig(Duration::ZERO).await?;
    for (method, path) in STEP_UP {
        let reply = rig.call(method, path).browser(&browser).send().await?;
        assert!(reauth_body(&reply)?, "{method} {path}: {}", reply.text());
        assert!(reply.header("www-authenticate").is_some());
    }
    let plain = rig
        .call("GET", "/api/memory/stats")
        .browser(&browser)
        .send()
        .await?;
    assert_eq!(plain.status, StatusCode::OK);
    let none = rig.call("PATCH", "/api/account").send().await?;
    assert!(!reauth_body(&none)?, "an anonymous caller is plainly 401");
    assert_eq!(none.status, StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn a_fresh_session_passes_every_sensitive_route() -> TestResult {
    let (rig, browser) = reauth_rig(Duration::from_secs(300)).await?;
    for (method, path) in STEP_UP {
        let reply = rig.call(method, path).browser(&browser).send().await?;
        assert!(!reauth_body(&reply)?, "{method} {path}");
        assert_ne!(reply.status, StatusCode::UNAUTHORIZED, "{method} {path}");
        if path == "/api/auth/rotate" {
            assert_eq!(reply.status, StatusCode::OK);
            break;
        }
    }
    Ok(())
}

#[tokio::test]
async fn the_owner_token_is_always_fresh() -> TestResult {
    let (rig, _browser) = reauth_rig(Duration::ZERO).await?;
    for (method, path) in STEP_UP
        .iter()
        .filter(|(_, path)| *path != "/api/auth/rotate")
    {
        let reply = rig.call(method, path).owner().send().await?;
        assert_ne!(reply.status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
    let rotated = rig.call("POST", "/api/auth/rotate").owner().send().await?;
    assert_eq!(rotated.status, StatusCode::OK);
    Ok(())
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn no_secret_reaches_the_logs_or_a_debug_string() -> TestResult {
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(captured.clone())
        .finish();
    let _guard = common::capture_logs(subscriber);

    let rig = rig()?;
    let old_token = rig.token();
    let code = mint(&rig).await?;
    let failed = exchange(&rig, "an-unknown-code-value").await?;
    assert_eq!(failed.status, StatusCode::UNAUTHORIZED);
    let reply = exchange(&rig, &code).await?;
    let browser = browser_of(&reply)?;
    exchange(&rig, &code).await?;
    rig.call("GET", "/api/memory/stats")
        .header("cookie", &format!("{COOKIE}={}", browser.session))
        .header("x-rh-csrf", "a-wrong-csrf-value")
        .send()
        .await?;
    rig.call("GET", "/api/memory/stats")
        .header("authorization", "Bearer a-wrong-bearer-value")
        .send()
        .await?;
    let pending = mint(&rig).await?;
    rig.call("POST", "/api/auth/rotate")
        .browser(&browser)
        .send()
        .await?;
    let rotated = rig.call("POST", "/api/auth/rotate").owner().send().await?;
    assert_eq!(rotated.status, StatusCode::OK);
    let new_token = rig.token();
    rig.call("POST", "/api/auth/logout")
        .browser(&browser)
        .send()
        .await?;

    let body: SessionResponse = reply.json()?;
    let request = SessionRequest {
        code: Secret::new(code.clone()),
    };
    let debugged = format!("{body:?} {request:?} {:?}", rig.owner);
    let logs = String::from_utf8_lossy(&captured.0.lock()).into_owned();
    assert!(logs.len() > 100, "the capture saw nothing: {logs}");
    for (name, secret) in [
        ("old token", old_token.as_str()),
        ("new token", new_token.as_str()),
        ("code", code.as_str()),
        ("pending code", pending.as_str()),
        ("csrf", browser.csrf.as_str()),
        ("session id", browser.session.as_str()),
        ("wrong code", "an-unknown-code-value"),
        ("wrong csrf", "a-wrong-csrf-value"),
        ("wrong bearer", "a-wrong-bearer-value"),
    ] {
        assert!(!logs.contains(secret), "{name} reached the logs");
        assert!(!debugged.contains(secret), "{name} reached a Debug string");
    }
    Ok(())
}

const AUTH_EVENTS: [&str; 10] = [
    "login_code_minted",
    "login_code_refused",
    "login_code_rate_limited",
    "exchange_succeeded",
    "exchange_refused",
    "exchange_rate_limited",
    "logout",
    "rotated",
    "bearer_rejected",
    "forwarded_token_refused",
];

struct Driven {
    old_token: String,
    code: String,
    browser: Browser,
}

async fn drive_every_auth_event(rig: &Rig) -> TestResult<Driven> {
    let old_token = rig.token();
    let code = mint(rig).await?;
    exchange(rig, "wrong-code-value").await?;
    let browser = browser_of(&exchange(rig, &code).await?)?;
    let by_session = rig
        .call("POST", "/api/auth/login-codes")
        .browser(&browser)
        .send()
        .await?;
    assert_eq!(by_session.status, StatusCode::UNAUTHORIZED);
    exchange(rig, "wrong-code-value").await?;
    let limited = exchange(rig, "wrong-code-value").await?;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    let mint_limited = rig
        .call("POST", "/api/auth/login-codes")
        .browser(&browser)
        .send()
        .await?;
    assert_eq!(mint_limited.status, StatusCode::TOO_MANY_REQUESTS);
    let forwarded = rig
        .call("GET", "/api/memory/stats")
        .owner()
        .header("x-forwarded-for", "203.0.113.9")
        .send()
        .await?;
    assert_eq!(forwarded.status, StatusCode::UNAUTHORIZED);
    rig.call("POST", "/api/auth/logout")
        .browser(&browser)
        .send()
        .await?;
    let rotated = rig.call("POST", "/api/auth/rotate").owner().send().await?;
    assert_eq!(rotated.status, StatusCode::OK);
    for _ in 0..21 {
        rig.call("GET", "/api/memory/stats")
            .header("authorization", "Bearer a-wrong-bearer-value")
            .send()
            .await?;
    }
    Ok(Driven {
        old_token,
        code,
        browser,
    })
}

#[tokio::test]
async fn every_auth_event_is_logged_with_a_reason_and_the_peer_and_no_secret() -> TestResult {
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(captured.clone())
        .finish();
    let _guard = common::capture_logs(subscriber);
    let rig = rig_with(
        AuthPolicy {
            attempts_per_minute: NonZeroU32::new(3).ok_or("three")?,
            ..AuthPolicy::default()
        },
        false,
    )?;
    let driven = drive_every_auth_event(&rig).await?;
    let logs = String::from_utf8_lossy(&captured.0.lock()).into_owned();
    assert_eq!(
        logs.matches("event=\"bearer_rejected\"").count(),
        1,
        "a repeated 401 logs once per peer"
    );
    for name in AUTH_EVENTS {
        let line = logs
            .lines()
            .find(|line| line.contains(&format!("event=\"{name}\"")))
            .ok_or_else(|| format!("no {name} event in:\n{logs}"))?;
        assert!(line.contains("reason="), "{name}: {line}");
        assert!(line.contains("peer=127.0.0.1"), "{name}: {line}");
    }
    for secret in [
        driven.old_token.as_str(),
        rig.token().as_str(),
        driven.code.as_str(),
        driven.browser.csrf.as_str(),
        driven.browser.session.as_str(),
        "wrong-code-value",
        "a-wrong-bearer-value",
    ] {
        assert!(
            !logs.contains(secret),
            "a secret reached the logs: {secret}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn the_auth_routes_refuse_other_methods() -> TestResult {
    let rig = rig()?;
    for path in [
        "/api/auth/login-codes",
        "/api/auth/session",
        "/api/auth/logout",
        "/api/auth/rotate",
    ] {
        let reply = rig.call("GET", path).owner().send().await?;
        assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED, "{path}");
        let anonymous = rig.call("GET", path).send().await?;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED, "{path}");
    }
    Ok(())
}

#[test]
fn the_fresh_window_is_read_from_the_gateway_section() -> TestResult {
    let configured: config::Config = serde_yaml_ng::from_str("gateway:\n  fresh_login_secs: 45\n")?;
    assert_eq!(
        AuthPolicy::from_config(&configured).fresh,
        Duration::from_secs(45)
    );
    let defaults = AuthPolicy::from_config(&config::Config::default());
    assert_eq!(defaults.fresh, Duration::from_mins(5));
    assert_eq!(defaults.idle, Duration::from_hours(7 * 24));
    assert_eq!(defaults.absolute, Duration::from_hours(30 * 24));
    Ok(())
}
