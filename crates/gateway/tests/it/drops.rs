use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::TimeDelta;
use config::model::TrustedProxy;
use gateway::Gateway;
use gateway::drops::{DropId, Drops, DropsConfig, Rate, Rates};
use gateway::server::{BindSource, ServeError, ServeOptions};
use gateway::tenancy::{Tenancy, UserId};
use kernel::memory::MemoryStore;
use kernel::owner::Owner;
use kernel::tenancy::{
    SessionVisibility, Space, TenancyStore, TenancyStoreError, TopicVisibility, Viewer, Visibility,
    VisibilityDefault,
};
use kernel::topics::TopicId;
use reqwest::{Method, StatusCode};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use wire::gateway::drops::{DropRefusal, Uploaded};
use wire::gateway::users::{ScopedToken, TokenScope, UserToken};
use wire::session_id::SessionId;

use crate::common;

pub(crate) type Outcome = Result<(), Box<dyn std::error::Error>>;
pub(crate) type Failure = Box<dyn std::error::Error>;

const AGE: &[u8] = b"age-encryption.org/v1\n";
const MAX: u64 = 64 * 1024;
pub(crate) const REMOTE: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));

pub(crate) fn sealed(len: usize) -> Vec<u8> {
    let mut body = AGE.to_vec();
    body.extend((0..len).map(|index| u8::try_from(index % 251).unwrap_or(7)));
    body
}

pub(crate) fn fresh_id() -> String {
    "AAAAAAAAAAAAAAAAAAAAAA".to_owned()
}

struct Tls {
    _dir: tempfile::TempDir,
    cert: PathBuf,
    key: PathBuf,
    pem: String,
}

fn tls() -> Result<Tls, Failure> {
    let key_pair = rcgen::KeyPair::generate()?;
    let certificate =
        rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()])?.self_signed(&key_pair)?;
    let dir = tempfile::tempdir()?;
    let cert = dir.path().join("node.crt");
    let key = dir.path().join("node.key");
    std::fs::write(&cert, certificate.pem())?;
    std::fs::write(&key, key_pair.serialize_pem())?;
    Ok(Tls {
        _dir: dir,
        cert,
        key,
        pem: certificate.pem(),
    })
}

pub(crate) struct Node {
    _dir: tempfile::TempDir,
    _tls: Option<Tls>,
    root: PathBuf,
    pub(crate) base: String,
    pem: Option<String>,
    token: CancellationToken,
    server: tokio::task::JoinHandle<Result<(), ServeError>>,
}

#[derive(Clone, Copy)]
pub(crate) struct Shape {
    pub(crate) enabled: bool,
    pub(crate) secure: bool,
    pub(crate) per_user_drops: u32,
    pub(crate) ttl: TimeDelta,
    pub(crate) rates: Rates,
    pub(crate) trusted_proxies: &'static [&'static str],
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            enabled: true,
            secure: false,
            per_user_drops: 50,
            ttl: TimeDelta::days(7),
            rates: Rates::default(),
            trusted_proxies: &[],
        }
    }
}

struct DefaultVisibility;

fn no_visibility() -> TenancyStoreError {
    TenancyStoreError::Unavailable("the drop tests keep no visibility".into())
}

#[async_trait::async_trait]
impl TenancyStore for DefaultVisibility {
    async fn sync_spaces(&self, _spaces: &[Space]) -> Result<(), TenancyStoreError> {
        Ok(())
    }

    async fn default_visibility(
        &self,
        _user: &Owner,
    ) -> Result<Option<VisibilityDefault>, TenancyStoreError> {
        Ok(None)
    }

    async fn set_default_visibility(
        &self,
        _user: &Owner,
        _visibility: &Visibility,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<VisibilityDefault, TenancyStoreError> {
        Err(no_visibility())
    }

    async fn session_visibility(
        &self,
        _viewer: &Viewer,
        _sessions: &[SessionId],
    ) -> Result<BTreeMap<SessionId, SessionVisibility>, TenancyStoreError> {
        Err(no_visibility())
    }

    async fn mark_session(
        &self,
        _viewer: &Viewer,
        _session: &SessionId,
        _mark: Option<&Visibility>,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<SessionVisibility, TenancyStoreError> {
        Err(no_visibility())
    }

    async fn mark_topic(
        &self,
        _viewer: &Viewer,
        _topic: TopicId,
        _mark: Option<&Visibility>,
    ) -> Result<TopicVisibility, TenancyStoreError> {
        Err(no_visibility())
    }

    async fn viewer_of(&self, _owner: &Owner) -> Result<Viewer, TenancyStoreError> {
        Err(no_visibility())
    }

    async fn conversation_owner(
        &self,
        _conversation: uuid::Uuid,
    ) -> Result<Option<Owner>, TenancyStoreError> {
        Err(no_visibility())
    }
}

fn tenancy() -> Result<Tenancy, Failure> {
    let user = |name: &str| UserId::new(name.to_owned()).map_err(|error| error.to_string());
    Ok(Tenancy::local(user(testkit::OWNER)?)
        .with_token("alice-token", user("alice")?)
        .map_err(|error| error.to_string())?
        .with_token("bob-token", user("bob")?)
        .map_err(|error| error.to_string())?
        .with_token("carol-token", user("carol")?)
        .map_err(|error| error.to_string())?
        .with_token("dave-token", user("dave")?)
        .map_err(|error| error.to_string())?
        .with_users_file_token(&drops_only("erin-token"), user("erin")?)
        .map_err(|error| error.to_string())?)
}

fn drops_only(secret: &str) -> UserToken {
    UserToken::Scoped(ScopedToken {
        digest: hex::encode(Sha256::digest(secret.as_bytes())),
        scopes: vec![TokenScope::DropsWrite],
    })
}

impl Node {
    pub(crate) async fn start(shape: Shape) -> Result<Self, Failure> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("drops");
        let store: Arc<dyn MemoryStore> = Arc::new(testkit::FakeMemoryStore::default());
        let trusted = shape
            .trusted_proxies
            .iter()
            .map(|entry| TrustedProxy::try_from((*entry).to_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut node = Gateway::new(store, common::settings())
            .with_tenancy(tenancy()?)
            .with_tenancy_store(Arc::new(DefaultVisibility))
            .with_trusted_proxies(trusted);
        if shape.enabled {
            let config = DropsConfig {
                root: root.clone(),
                max_bytes: MAX,
                per_user_drops: shape.per_user_drops,
                per_user_bytes: 512 << 20,
                ttl: shape.ttl,
                rates: shape.rates,
            };
            node = node.with_drops(Arc::new(Drops::open(&config)?));
        }
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let material = if shape.secure { Some(tls()?) } else { None };
        let (tls_paths, pem, scheme) = if let Some(material) = &material {
            let paths = gateway::tls::resolve_tls(&BindSource {
                host: "127.0.0.1".to_owned(),
                port: addr.port(),
                tls_cert: Some(material.cert.clone()),
                tls_key: Some(material.key.clone()),
                notices: Vec::new(),
            })?
            .ok_or("tls paths")?;
            (Some(paths), Some(material.pem.clone()), "https")
        } else {
            (None, None, "http")
        };
        let token = CancellationToken::new();
        let server = tokio::spawn(gateway::server::serve_with(
            listener,
            Arc::new(node),
            tls_paths,
            token.clone(),
            ServeOptions::default(),
        ));
        Ok(Self {
            _dir: dir,
            _tls: material,
            root,
            base: format!("{scheme}://{addr}"),
            pem,
            token,
            server,
        })
    }

    pub(crate) fn client(&self, from: IpAddr) -> Result<reqwest::Client, Failure> {
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .local_address(from)
            .timeout(Duration::from_secs(30));
        if let Some(pem) = &self.pem {
            builder = builder.add_root_certificate(reqwest::Certificate::from_pem(pem.as_bytes())?);
        }
        Ok(builder.build()?)
    }

    fn files(&self) -> Result<Vec<String>, Failure> {
        let mut names = std::fs::read_dir(&self.root)?
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>, std::io::Error>>()?;
        names.sort();
        Ok(names)
    }

    pub(crate) async fn stop(self) -> Outcome {
        self.token.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.server).await???;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Payload {
    Empty,
    Bytes(Vec<u8>),
    Chunked(Vec<u8>),
}

#[derive(Debug)]
pub(crate) struct Answer {
    pub(crate) status: StatusCode,
    headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Answer {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn refusal(&self) -> Result<DropRefusal, Failure> {
        Ok(serde_json::from_slice(&self.body)?)
    }
}

pub(crate) async fn call(
    client: &reqwest::Client,
    method: Method,
    url: &str,
    auth: Option<&str>,
    payload: Payload,
) -> Result<Answer, Failure> {
    let auth: Vec<(&str, &str)> = auth
        .map(|auth| ("authorization", auth))
        .into_iter()
        .collect();
    send(client, method, url, &auth, payload).await
}

pub(crate) async fn send(
    client: &reqwest::Client,
    method: Method,
    url: &str,
    headers: &[(&str, &str)],
    payload: Payload,
) -> Result<Answer, Failure> {
    let mut request = client.request(method, url);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request = match payload {
        Payload::Empty => request,
        Payload::Bytes(bytes) => request.body(bytes),
        Payload::Chunked(bytes) => {
            let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
                bytes.chunks(4096).map(|chunk| Ok(chunk.to_vec())).collect();
            request.body(reqwest::Body::wrap_stream(futures::stream::iter(chunks)))
        }
    };
    let response = request.send().await?;
    let status = response.status();
    let mut headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .filter(|(name, _)| *name != "date")
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    headers.sort();
    let body = response.bytes().await?.to_vec();
    Ok(Answer {
        status,
        headers,
        body,
    })
}

pub(crate) async fn upload(node: &Node, token: &str, body: Vec<u8>) -> Result<String, Failure> {
    let answer = call(
        &node.client(IpAddr::V4(Ipv4Addr::LOCALHOST))?,
        Method::POST,
        &format!("{}/api/drops", node.base),
        Some(&format!("Bearer {token}")),
        Payload::Bytes(body),
    )
    .await?;
    assert_eq!(answer.status, StatusCode::CREATED, "{:?}", answer.refusal());
    Ok(serde_json::from_slice::<Uploaded>(&answer.body)?.id)
}

struct Case {
    label: &'static str,
    method: Method,
    path: String,
    auth: Option<&'static str>,
    payload: Payload,
    status: StatusCode,
    body: Option<DropRefusal>,
    headers: &'static [(&'static str, &'static str)],
}

fn case(label: &'static str, method: Method, path: &str, status: StatusCode) -> Case {
    Case {
        label,
        method,
        path: path.to_owned(),
        auth: None,
        payload: Payload::Empty,
        status,
        body: None,
        headers: &[],
    }
}

impl Case {
    fn auth(mut self, auth: &'static str) -> Self {
        self.auth = Some(auth);
        self
    }

    fn payload(mut self, payload: Payload) -> Self {
        self.payload = payload;
        self
    }

    fn refused(mut self, error: &str) -> Self {
        self.body = Some(DropRefusal {
            error: error.to_owned(),
            status: self.status.as_u16(),
        });
        self
    }

    fn headers(mut self, headers: &'static [(&'static str, &'static str)]) -> Self {
        self.headers = headers;
        self
    }
}

pub(crate) fn refused(error: &str, status: u16) -> DropRefusal {
    DropRefusal {
        error: error.to_owned(),
        status,
    }
}

const NO_CREDENTIALS_UNLIKE_CLOUD: &[(&str, &str)] = &[(
    "www-authenticate",
    r#"Bearer realm="rivethub", scope="drops:write""#,
)];

const INVALID_TOKEN: &[(&str, &str)] = &[(
    "www-authenticate",
    r#"Bearer realm="rivethub", error="invalid_token", scope="drops:write""#,
)];

fn auth_cases() -> Vec<Case> {
    vec![
        case(
            "no token, challenged without an error attribute unlike the cloud",
            Method::POST,
            "/api/drops",
            StatusCode::UNAUTHORIZED,
        )
        .payload(Payload::Bytes(sealed(16)))
        .refused("invalid_token")
        .headers(NO_CREDENTIALS_UNLIKE_CLOUD),
        case(
            "query token",
            Method::POST,
            "/api/drops?access_token=alice-token",
            StatusCode::UNAUTHORIZED,
        )
        .payload(Payload::Bytes(sealed(16)))
        .refused("invalid_token")
        .headers(NO_CREDENTIALS_UNLIKE_CLOUD),
        case(
            "unknown token",
            Method::POST,
            "/api/drops",
            StatusCode::UNAUTHORIZED,
        )
        .auth("Bearer rhca_wrong")
        .payload(Payload::Bytes(sealed(16)))
        .refused("invalid_token")
        .headers(INVALID_TOKEN),
        case(
            "drops-only token, never the cloud's 403 because every token holds drops:write",
            Method::POST,
            "/api/drops",
            StatusCode::CREATED,
        )
        .auth("Bearer erin-token")
        .payload(Payload::Bytes(sealed(16))),
        case(
            "lowercase scheme",
            Method::POST,
            "/api/drops",
            StatusCode::CREATED,
        )
        .auth("bearer carol-token")
        .payload(Payload::Bytes(sealed(16))),
        case(
            "uppercase scheme",
            Method::POST,
            "/api/drops",
            StatusCode::CREATED,
        )
        .auth("BEARER carol-token")
        .payload(Payload::Bytes(sealed(16))),
    ]
}

fn body_cases() -> Result<Vec<Case>, Failure> {
    Ok(vec![
        case(
            "json body",
            Method::POST,
            "/api/drops",
            StatusCode::BAD_REQUEST,
        )
        .auth("Bearer bob-token")
        .payload(Payload::Bytes(br#"{"not":"age"}"#.to_vec()))
        .refused("body is not an age file"),
        case(
            "empty body",
            Method::POST,
            "/api/drops",
            StatusCode::BAD_REQUEST,
        )
        .auth("Bearer bob-token")
        .payload(Payload::Bytes(Vec::new()))
        .refused("body is not an age file"),
        case(
            "age v2",
            Method::POST,
            "/api/drops",
            StatusCode::BAD_REQUEST,
        )
        .auth("Bearer bob-token")
        .payload(Payload::Bytes(b"age-encryption.org/v2\n".to_vec()))
        .refused("body is not an age file"),
        case(
            "declared too large",
            Method::POST,
            "/api/drops",
            StatusCode::PAYLOAD_TOO_LARGE,
        )
        .auth("Bearer bob-token")
        .payload(Payload::Bytes(sealed(usize::try_from(MAX)?)))
        .refused("payload too large"),
    ])
}

fn quota_cases() -> Vec<Case> {
    vec![
        case(
            "first drop",
            Method::POST,
            "/api/drops",
            StatusCode::CREATED,
        )
        .auth("Bearer alice-token")
        .payload(Payload::Bytes(sealed(16))),
        case(
            "second drop",
            Method::POST,
            "/api/drops",
            StatusCode::CREATED,
        )
        .auth("Bearer alice-token")
        .payload(Payload::Bytes(sealed(16))),
        case(
            "over quota",
            Method::POST,
            "/api/drops",
            StatusCode::TOO_MANY_REQUESTS,
        )
        .auth("Bearer alice-token")
        .payload(Payload::Bytes(sealed(16)))
        .refused("drop quota exceeded"),
        case(
            "over rate",
            Method::POST,
            "/api/drops",
            StatusCode::TOO_MANY_REQUESTS,
        )
        .auth("Bearer alice-token")
        .payload(Payload::Bytes(sealed(16)))
        .refused("rate limit exceeded"),
    ]
}

fn routes_cases() -> Vec<Case> {
    let item = format!("/api/drops/{}", fresh_id());
    vec![
        case("unknown id", Method::GET, &item, StatusCode::NOT_FOUND).refused("not found"),
        case(
            "malformed id",
            Method::GET,
            "/api/drops/short",
            StatusCode::NOT_FOUND,
        )
        .refused("not found"),
        case(
            "revoke without token",
            Method::DELETE,
            &item,
            StatusCode::UNAUTHORIZED,
        )
        .refused("invalid_token")
        .headers(NO_CREDENTIALS_UNLIKE_CLOUD),
        case(
            "revoke unknown",
            Method::DELETE,
            &item,
            StatusCode::NOT_FOUND,
        )
        .auth("Bearer alice-token")
        .refused("not found"),
        case(
            "list drops",
            Method::GET,
            "/api/drops",
            StatusCode::METHOD_NOT_ALLOWED,
        )
        .refused("method not allowed"),
        case(
            "put drop",
            Method::PUT,
            &item,
            StatusCode::METHOD_NOT_ALLOWED,
        )
        .refused("method not allowed"),
        case(
            "head drop",
            Method::HEAD,
            &item,
            StatusCode::METHOD_NOT_ALLOWED,
        ),
    ]
}

fn cloud_cases() -> Result<Vec<Case>, Failure> {
    let groups = [auth_cases(), body_cases()?, quota_cases(), routes_cases()];
    Ok(groups.into_iter().flatten().collect())
}

fn check_created(answer: &Answer, label: &str) -> Outcome {
    let created: Uploaded = serde_json::from_slice(&answer.body)?;
    assert!(
        created.id.parse::<DropId>().is_ok(),
        "{label}: {:?}",
        created.id
    );
    assert_eq!(created.bytes, AGE.len() as u64 + 16, "{label}");
    let left = created.expires_at - chrono::Utc::now();
    assert!(
        left > TimeDelta::days(7) - TimeDelta::minutes(1) && left <= TimeDelta::days(7),
        "{label}: {left}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn same_api_as_cloud() -> Outcome {
    let node = Node::start(Shape {
        per_user_drops: 2,
        rates: Rates {
            uploads_per_user: Rate::per(3, Duration::from_secs(3600)),
            ..Rates::default()
        },
        ..Shape::default()
    })
    .await?;
    let cases = cloud_cases()?;
    let client = node.client(IpAddr::V4(Ipv4Addr::LOCALHOST))?;
    for case in cases {
        let answer = call(
            &client,
            case.method.clone(),
            &format!("{}{}", node.base, case.path),
            case.auth,
            case.payload.clone(),
        )
        .await?;
        assert_eq!(answer.status, case.status, "{}", case.label);
        assert_eq!(
            answer.header("cache-control"),
            Some("no-store"),
            "{}",
            case.label
        );
        if let Some(body) = &case.body {
            assert_eq!(&answer.refusal()?, body, "{}", case.label);
        }
        for (name, value) in case.headers {
            assert_eq!(answer.header(name), Some(*value), "{}", case.label);
        }
        if case.status == StatusCode::CREATED {
            check_created(&answer, case.label)?;
        }
    }
    let body = sealed(10_000);
    let id = upload(&node, "dave-token", body.clone()).await?;
    let url = format!("{}/api/drops/{id}", node.base);
    let first = call(&client, Method::GET, &url, None, Payload::Empty).await?;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(
        first.header("content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(first.header("cache-control"), Some("no-store"));
    assert_eq!(
        first.header("content-length"),
        Some(body.len().to_string().as_str())
    );
    assert_eq!(first.body, body);
    let second = call(&client, Method::GET, &url, None, Payload::Empty).await?;
    assert_eq!(second.status, StatusCode::NOT_FOUND);
    assert_eq!(second.refusal()?, refused("not found", 404));
    node.stop().await
}

async fn remote_reads_are_refused_while_drops_are_off() -> Outcome {
    let off = Node::start(Shape {
        enabled: false,
        ..Shape::default()
    })
    .await?;
    let remote = off.client(REMOTE)?;
    let item = format!("{}/api/drops/{}", off.base, fresh_id());
    let refused = call(&remote, Method::GET, &item, None, Payload::Empty).await?;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    off.stop().await
}

async fn cleartext_remote_writes_stop_at_the_guard() -> Outcome {
    let on = Node::start(Shape::default()).await?;
    let remote = on.client(REMOTE)?;
    let body = sealed(512);
    let id = upload(&on, "alice-token", body.clone()).await?;
    let got = call(
        &remote,
        Method::GET,
        &format!("{}/api/drops/{id}", on.base),
        None,
        Payload::Empty,
    )
    .await?;
    assert_eq!((got.status, got.body), (StatusCode::OK, body));
    let missing = call(
        &remote,
        Method::GET,
        &format!("{}/api/drops/{}", on.base, fresh_id()),
        None,
        Payload::Empty,
    )
    .await?;
    assert_eq!(missing.refusal()?, refused("not found", 404));
    let cleartext = call(
        &remote,
        Method::POST,
        &format!("{}/api/drops", on.base),
        Some("Bearer alice-token"),
        Payload::Bytes(sealed(16)),
    )
    .await?;
    assert_eq!(cleartext.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        cleartext.header("www-authenticate"),
        Some(r#"Bearer realm="rivethub", error="invalid_token""#)
    );
    let id = upload(&on, "alice-token", sealed(16)).await?;
    let revoke = call(
        &remote,
        Method::DELETE,
        &format!("{}/api/drops/{id}", on.base),
        Some("Bearer alice-token"),
        Payload::Empty,
    )
    .await?;
    assert_eq!(revoke.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        revoke.header("www-authenticate"),
        Some(r#"Bearer realm="rivethub", error="invalid_token""#)
    );
    assert_eq!(on.files()?.len(), 2);
    on.stop().await
}

async fn tls_remote_writes_reach_the_handler() -> Outcome {
    let secure = Node::start(Shape {
        secure: true,
        ..Shape::default()
    })
    .await?;
    let remote = secure.client(REMOTE)?;
    let url = format!("{}/api/drops", secure.base);
    let anonymous = call(
        &remote,
        Method::POST,
        &url,
        None,
        Payload::Bytes(sealed(16)),
    )
    .await?;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(anonymous.refusal()?, refused("invalid_token", 401));
    let sent = call(
        &remote,
        Method::POST,
        &url,
        Some("Bearer alice-token"),
        Payload::Bytes(sealed(16)),
    )
    .await?;
    assert_eq!(sent.status, StatusCode::CREATED);
    secure.stop().await
}

#[tokio::test(flavor = "multi_thread")]
async fn anonymous_get_only_when_enabled() -> Outcome {
    remote_reads_are_refused_while_drops_are_off().await?;
    cleartext_remote_writes_stop_at_the_guard().await?;
    tls_remote_writes_reach_the_handler().await
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_gets_yield_one_body() -> Outcome {
    let node = Node::start(Shape {
        rates: Rates {
            misses_per_ip: Rate::per(100, Duration::from_secs(600)),
            ..Rates::default()
        },
        ..Shape::default()
    })
    .await?;
    let body = sealed(48 * 1024);
    let id = upload(&node, "alice-token", body.clone()).await?;
    let client = node.client(IpAddr::V4(Ipv4Addr::LOCALHOST))?;
    let url = format!("{}/api/drops/{id}", node.base);
    let answers = futures::future::join_all(
        (0..20).map(|_| call(&client, Method::GET, &url, None, Payload::Empty)),
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    let served: Vec<&Answer> = answers
        .iter()
        .filter(|answer| answer.status == StatusCode::OK)
        .collect();
    assert_eq!(served.len(), 1);
    assert_eq!(served.first().ok_or("one body")?.body, body);
    assert_eq!(
        answers
            .iter()
            .filter(|answer| answer.status == StatusCode::NOT_FOUND)
            .count(),
        19
    );
    assert_eq!(node.files()?, Vec::<String>::new());
    node.stop().await
}

#[tokio::test(flavor = "multi_thread")]
async fn non_age_and_oversize_refused_without_files() -> Outcome {
    let node = Node::start(Shape::default()).await?;
    let client = node.client(IpAddr::V4(Ipv4Addr::LOCALHOST))?;
    let url = format!("{}/api/drops", node.base);
    let too_large = sealed(usize::try_from(MAX)?);
    let mut late_garbage = sealed(8);
    late_garbage.splice(3..4, *b"X");
    for (payload, status) in [
        (
            Payload::Bytes(b"{\"not\":\"age\"}".to_vec()),
            StatusCode::BAD_REQUEST,
        ),
        (Payload::Bytes(Vec::new()), StatusCode::BAD_REQUEST),
        (
            Payload::Bytes(AGE.get(..10).ok_or("age")?.to_vec()),
            StatusCode::BAD_REQUEST,
        ),
        (Payload::Bytes(late_garbage), StatusCode::BAD_REQUEST),
        (
            Payload::Chunked(b"PK\x03\x04 zip".repeat(2000)),
            StatusCode::BAD_REQUEST,
        ),
        (
            Payload::Bytes(too_large.clone()),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
        (Payload::Chunked(too_large), StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let answer = call(
            &client,
            Method::POST,
            &url,
            Some("Bearer alice-token"),
            payload,
        )
        .await?;
        assert_eq!(answer.status, status);
        assert_eq!(node.files()?, Vec::<String>::new());
    }
    let id = upload(&node, "alice-token", sealed(16)).await?;
    assert_eq!(
        node.files()?,
        vec![format!("{id}.age"), format!("{id}.json")]
    );
    let mode = |name: &str| -> Result<u32, Failure> {
        use std::os::unix::fs::PermissionsExt as _;
        Ok(std::fs::metadata(node.root.join(name))?
            .permissions()
            .mode()
            & 0o777)
    };
    assert_eq!(mode(&format!("{id}.age"))?, 0o600);
    assert_eq!(mode(&format!("{id}.json"))?, 0o600);
    node.stop().await
}

#[tokio::test(flavor = "multi_thread")]
async fn revoke_needs_the_owner() -> Outcome {
    let node = Node::start(Shape::default()).await?;
    let client = node.client(IpAddr::V4(Ipv4Addr::LOCALHOST))?;
    let id = upload(&node, "alice-token", sealed(64)).await?;
    let url = format!("{}/api/drops/{id}", node.base);
    let revoke = |auth: Option<&'static str>, url: String| {
        let client = client.clone();
        async move { call(&client, Method::DELETE, &url, auth, Payload::Empty).await }
    };
    assert_eq!(
        revoke(Some("Bearer bob-token"), url.clone()).await?.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        revoke(None, url.clone()).await?.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        revoke(
            Some("Bearer alice-token"),
            format!("{}/api/drops/nope", node.base)
        )
        .await?
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(node.files()?.len(), 2);
    let done = revoke(Some("Bearer alice-token"), url.clone()).await?;
    assert_eq!(done.status, StatusCode::NO_CONTENT);
    assert_eq!(done.header("cache-control"), Some("no-store"));
    assert_eq!(
        call(&client, Method::GET, &url, None, Payload::Empty)
            .await?
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(node.files()?, Vec::<String>::new());
    node.stop().await
}

async fn shape_of(node: &Node, id: &str) -> Result<Answer, Failure> {
    call(
        &node.client(IpAddr::V4(Ipv4Addr::LOCALHOST))?,
        Method::GET,
        &format!("{}/api/drops/{id}", node.base),
        None,
        Payload::Empty,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_and_unknown_look_identical() -> Outcome {
    let live = Node::start(Shape::default()).await?;
    let consumed = upload(&live, "alice-token", sealed(32)).await?;
    assert_eq!(shape_of(&live, &consumed).await?.status, StatusCode::OK);
    let expired_node = Node::start(Shape {
        ttl: TimeDelta::zero(),
        ..Shape::default()
    })
    .await?;
    let expired = upload(&expired_node, "alice-token", sealed(32)).await?;
    let reference = shape_of(&expired_node, &expired).await?;
    assert_eq!(reference.status, StatusCode::NOT_FOUND);
    assert_eq!(reference.refusal()?, refused("not found", 404));
    assert_eq!(expired_node.files()?, Vec::<String>::new());
    for id in [
        consumed,
        fresh_id(),
        "short".to_owned(),
        "a%2Fb".to_owned(),
        "%FF".to_owned(),
    ] {
        let answer = shape_of(&live, &id).await?;
        assert_eq!(answer.status, reference.status);
        assert_eq!(answer.headers, reference.headers);
        assert_eq!(answer.body, reference.body);
    }
    expired_node.stop().await?;
    live.stop().await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_id_that_is_not_utf8_counts_as_a_miss() -> Outcome {
    let node = Node::start(Shape::default()).await?;
    let id = upload(&node, "alice-token", sealed(16)).await?;
    for _ in 0..10 {
        assert_eq!(shape_of(&node, "%FF").await?.status, StatusCode::NOT_FOUND);
    }
    assert_eq!(
        shape_of(&node, &id).await?.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    let revoked = call(
        &node.client(IpAddr::V4(Ipv4Addr::LOCALHOST))?,
        Method::DELETE,
        &format!("{}/api/drops/%FF", node.base),
        Some("Bearer alice-token"),
        Payload::Empty,
    )
    .await?;
    assert_eq!(revoked.status, StatusCode::NOT_FOUND);
    assert_eq!(revoked.refusal()?, refused("not found", 404));
    node.stop().await
}

#[tokio::test(flavor = "multi_thread")]
async fn ten_misses_close_drop_reads_from_that_address() -> Outcome {
    let node = Node::start(Shape::default()).await?;
    let id = upload(&node, "alice-token", sealed(16)).await?;
    for _ in 0..10 {
        assert_eq!(
            shape_of(&node, &fresh_id()).await?.status,
            StatusCode::NOT_FOUND
        );
    }
    let limited = shape_of(&node, &id).await?;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.refusal()?, refused("rate limit exceeded", 429));
    assert!(limited.header("retry-after").is_some());
    let other = call(
        &node.client(REMOTE)?,
        Method::GET,
        &format!("{}/api/drops/{id}", node.base),
        None,
        Payload::Empty,
    )
    .await?;
    assert_eq!(other.status, StatusCode::OK);
    node.stop().await
}
