use crate::support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cli::Exit;
use gateway::server::BindSource;
use gateway::tenancy::{Tenancy, UserId};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use support::{config, environment, home, run};
use testkit::{FakeMemoryStore, MemoryRequest};
use tokio_util::sync::CancellationToken;

const PROMPT: &str = r#"{"session_id":"55555555-5555-5555-5555-555555555555","cwd":"/work/demo","hook_event_name":"UserPromptSubmit","prompt":"list the files"}"#;
const TOOL: &str = r#"{"session_id":"55555555-5555-5555-5555-555555555555","cwd":"/work/demo","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"toolu_1"}"#;

fn now() -> Result<chrono::DateTime<chrono::Utc>, chrono::ParseError> {
    "2026-10-10T12:00:00Z".parse()
}

#[tokio::test]
async fn the_hook_never_fails_the_harness_without_a_config()
-> Result<(), Box<dyn std::error::Error>> {
    let home = home();
    let env = environment(&home);
    let report = cli::capture_hook::hook(&env, PROMPT, now()?).await;
    assert_eq!(report.exit, Exit::Ok);
    assert_eq!(report.stdout, "");
    assert!(
        report
            .stderr
            .starts_with("capture hook is not configured: "),
        "{}",
        report.stderr
    );
    Ok(())
}

#[tokio::test]
async fn the_capture_command_lists_the_hook_subcommand() {
    let home = home();
    let env = environment(&home);
    let report = run(&["capture", "--help"], &env).await;
    assert_eq!(report.exit, Exit::Ok);
    assert!(
        report
            .stdout
            .contains("Capture one harness hook payload read from stdin")
            && report.stdout.contains("spool"),
        "{}",
        report.stdout
    );
    assert!(report.stdout.contains("backfill"), "{}", report.stdout);
}

fn store_a_session(home: &tempfile::TempDir) -> std::io::Result<()> {
    let slug = home.path().join(".claude/projects/-work-demo");
    std::fs::create_dir_all(&slug)?;
    std::fs::write(
        slug.join("55555555-5555-5555-5555-555555555555.jsonl"),
        concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-10-10T12:00:00.000Z","message":{"role":"user","content":"say hello"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-10-10T12:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
            "\n"
        ),
    )
}

#[tokio::test]
async fn backfill_reports_what_it_would_capture_and_posts_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    let home = home();
    let env = environment(&home);
    config(&home, "gateway:\n  host: 127.0.0.1\n  port: 1\n");
    store_a_session(&home)?;
    let report = run(
        &[
            "capture",
            "backfill",
            "--dry-run",
            "--harness",
            "claude-code",
        ],
        &env,
    )
    .await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(
        report.stdout,
        "claude-code: 1 sessions, 2 messages read, none posted, 0 failures\n"
    );
    assert!(!home.path().join(".rivethub/capture-spool").exists());
    Ok(())
}

#[tokio::test]
async fn backfill_exits_1_and_names_the_failure_when_the_gateway_is_unreachable()
-> Result<(), Box<dyn std::error::Error>> {
    let port = testkit::ports::claim()?;
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    store_a_session(&home)?;
    let report = run(&["capture", "backfill", "--harness", "claude-code"], &env).await;
    assert_eq!(report.exit, Exit::Failure);
    assert_eq!(
        report.stdout,
        "claude-code: 1 sessions, 0 messages added, 1 failures\n"
    );
    assert!(
        report.stderr.starts_with(
            "claude-code 55555555-5555-5555-5555-555555555555: capture request failed"
        ),
        "{}",
        report.stderr
    );
    Ok(())
}

#[tokio::test]
async fn a_users_file_token_captures_through_a_remote_gateway_as_its_user()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let home = home();
    let env = environment(&home);
    let tls = home.path().join("tls");
    std::fs::create_dir_all(&tls)?;
    let root_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    let mut root = CertificateParams::new(Vec::<String>::new())?;
    root.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    root.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    std::fs::write(tls.join("ca.pem"), root.self_signed(&root_key)?.pem())?;
    let issuer = Issuer::from_params(&root, &root_key);
    let mut host = CertificateParams::new(vec!["127.0.0.1".to_owned()])?;
    host.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf(&tls, "server", &host, &issuer)?;

    let store = Arc::new(FakeMemoryStore::default());
    let tenancy = Tenancy::local(UserId::new("owner".to_owned())?)
        .with_token("member-token", UserId::new("member".to_owned())?)?;
    let gateway =
        gateway::Gateway::new(Arc::clone(&store) as _, settings(home.path())).with_tenancy(tenancy);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let tls_paths = gateway::tls::resolve_tls(&BindSource {
        host: "127.0.0.1".to_owned(),
        port,
        tls_cert: Some(tls.join("server.pem")),
        tls_key: Some(tls.join("server.key")),
        notices: Vec::new(),
    })?;
    let stop = CancellationToken::new();
    let server = tokio::spawn(gateway::server::serve(
        listener,
        Arc::new(gateway),
        tls_paths,
        stop.clone(),
    ));

    let own = testkit::ports::claim()?;
    config(
        &home,
        &format!(
            "gateway:\n  host: 127.0.0.1\n  port: {own}\nmemory:\n  capture:\n    gateway:\n      url: https://127.0.0.1:{port}\n      ca: {}\n      token: member-token\n",
            tls.join("ca.pem").display(),
        ),
    );
    let report = cli::capture_hook::hook(&env, TOOL, now()?).await;
    let owners: Vec<String> = store
        .requests()
        .await
        .into_iter()
        .filter_map(|request| {
            if let MemoryRequest::Append(owner, _) = request {
                Some(owner.as_str().to_owned())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(owners, ["member"], "{}", report.stderr);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await???;
    Ok(())
}

fn leaf(
    directory: &Path,
    name: &str,
    params: &CertificateParams,
    issuer: &Issuer<'_, &KeyPair>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    std::fs::write(
        directory.join(format!("{name}.pem")),
        params.signed_by(&key, issuer)?.pem(),
    )?;
    std::fs::write(directory.join(format!("{name}.key")), key.serialize_pem())?;
    Ok(())
}

pub(crate) fn settings(home: &Path) -> gateway::Settings {
    gateway::Settings {
        hostname: "gold-host".to_owned(),
        node: "golden-node".to_owned(),
        allowed_origins: Vec::new(),
        allowed_hosts: Vec::new(),
        static_dir: None,
        root_redirect: None,
        state_dir: home.join("gateway"),
        private_ca: None,
        request_deadline: gateway::REQUEST_DEADLINE,
        policy: None,
    }
}

pub(crate) async fn local_gateway(
    store: &Arc<FakeMemoryStore>,
    home: &Path,
    owner: Arc<gateway::local_owner::LocalOwner>,
) -> Result<
    (
        u16,
        CancellationToken,
        tokio::task::JoinHandle<Result<(), gateway::server::ServeError>>,
    ),
    Box<dyn std::error::Error + Send + Sync>,
> {
    local_gateway_with(store, home, owner, None).await
}

pub(crate) async fn local_gateway_with(
    store: &Arc<FakeMemoryStore>,
    home: &Path,
    owner: Arc<gateway::local_owner::LocalOwner>,
    static_dir: Option<std::path::PathBuf>,
) -> Result<
    (
        u16,
        CancellationToken,
        tokio::task::JoinHandle<Result<(), gateway::server::ServeError>>,
    ),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let mut settings = settings(home);
    settings.static_dir = static_dir;
    let gateway = gateway::Gateway::new(Arc::clone(store) as _, settings)
        .with_local_owner(owner)
        .with_tenancy(Tenancy::local(UserId::new("owner".to_owned())?));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let stop = CancellationToken::new();
    let server = tokio::spawn(gateway::server::serve(
        listener,
        Arc::new(gateway),
        None,
        stop.clone(),
    ));
    Ok((port, stop, server))
}

fn appended_owners(requests: Vec<MemoryRequest>) -> Vec<String> {
    requests
        .into_iter()
        .filter_map(|request| {
            if let MemoryRequest::Append(owner, _) = request {
                Some(owner.as_str().to_owned())
            } else {
                None
            }
        })
        .collect()
}

#[tokio::test]
async fn the_hook_captures_through_the_local_gateway_with_the_owner_token()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let home = home();
    let env = environment(&home);
    let store = Arc::new(FakeMemoryStore::default());
    let (port, stop, server) =
        local_gateway(&store, home.path(), support::local_owner(home.path())).await?;
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    let report = cli::capture_hook::hook(&env, TOOL, now()?).await;
    assert_eq!(report.stderr, "");
    assert_eq!(appended_owners(store.requests().await), ["owner"]);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await???;
    Ok(())
}

#[tokio::test]
async fn a_batch_spooled_on_401_replays_once_the_token_is_right()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let home = home();
    let env = environment(&home);
    let store = Arc::new(FakeMemoryStore::default());
    let owner = Arc::new(gateway::local_owner::LocalOwner::unpersisted()?);
    let (port, stop, server) = local_gateway(&store, home.path(), Arc::clone(&owner)).await?;
    support::local_owner(home.path());
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    cli::capture_hook::hook(&env, TOOL, now()?).await;
    assert_eq!(appended_owners(store.requests().await).len(), 0);
    support::install_owner_token(&env, &owner.current());
    cli::capture_hook::replay_spool(&env, now()?).await?;
    assert_eq!(appended_owners(store.requests().await), ["owner"]);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await???;
    Ok(())
}

#[tokio::test]
async fn backfill_sends_the_owner_token_and_a_rerun_adds_nothing()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let home = home();
    let env = environment(&home);
    let store = Arc::new(FakeMemoryStore::default());
    let (port, stop, server) =
        local_gateway(&store, home.path(), support::local_owner(home.path())).await?;
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    store_a_session(&home)?;
    let args = ["capture", "backfill", "--harness", "claude-code"];
    let first = run(&args, &env).await;
    assert_eq!(first.exit, Exit::Ok, "{}", first.stderr);
    assert_eq!(
        first.stdout,
        "claude-code: 1 sessions, 2 messages added, 0 failures\n"
    );
    assert_eq!(appended_owners(store.requests().await), ["owner"]);
    let second = run(&args, &env).await;
    assert_eq!(second.exit, Exit::Ok, "{}", second.stderr);
    assert_eq!(
        second.stdout,
        "claude-code: 1 sessions, 0 messages added, 0 failures\n"
    );
    assert_eq!(spool_entries(home.path()), 0);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await???;
    Ok(())
}

fn spool_entries(home: &Path) -> usize {
    std::fs::read_dir(home.join(".rivethub/capture-spool")).map_or(0, Iterator::count)
}

#[tokio::test]
async fn a_batch_survives_a_stopped_gateway_when_the_token_exists_and_the_port_never_sees_it()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port = testkit::ports::claim()?.number();
    let home = home();
    let env = environment(&home);
    let owner = support::local_owner(home.path());
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    let report = cli::capture_hook::hook(&env, TOOL, now()?).await;
    assert_eq!(report.exit, Exit::Ok);
    assert!(
        report
            .stderr
            .contains(&format!("the gateway is not running on port {port}")),
        "{}",
        report.stderr
    );
    assert_eq!(spool_entries(home.path()), 1);
    let store = Arc::new(FakeMemoryStore::default());
    let gateway = gateway::Gateway::new(Arc::clone(&store) as _, settings(home.path()))
        .with_local_owner(owner)
        .with_tenancy(Tenancy::local(UserId::new("owner".to_owned())?));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let stop = CancellationToken::new();
    let server = tokio::spawn(gateway::server::serve(
        listener,
        Arc::new(gateway),
        None,
        stop.clone(),
    ));
    let report = cli::capture_hook::hook(&env, TOOL, now()?).await;
    assert_eq!(report.stderr, "");
    assert_eq!(spool_entries(home.path()), 0);
    assert_eq!(appended_owners(store.requests().await), ["owner", "owner"]);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await???;
    Ok(())
}

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
async fn the_token_never_reaches_logs_or_error_text()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let logs = LogBuffer::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(logs.clone())
            .finish(),
    );
    let home = home();
    let env = environment(&home);
    let store = Arc::new(FakeMemoryStore::default());
    let owner = Arc::new(gateway::local_owner::LocalOwner::unpersisted()?);
    let (port, stop, server) = local_gateway(&store, home.path(), owner).await?;
    let token = support::local_owner(home.path()).current();
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    let mut text = cli::capture_hook::hook(&env, TOOL, now()?).await.stderr;
    let replayed_text = format!("{:?}", cli::capture_hook::replay_spool(&env, now()?).await);
    text.push_str(&replayed_text);
    text.push_str(&run(&["auth", "token-path"], &env).await.stdout);
    text.push_str(&String::from_utf8(logs.0.lock().clone())?);
    assert!(text.contains("401"), "{text}");
    assert!(!text.contains(token.expose()), "{text}");
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await???;
    Ok(())
}
