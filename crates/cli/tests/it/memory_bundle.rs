#![cfg(test)]

use crate::support;
use jiff::Timestamp;

use std::path::Path;

use cli::Exit;
use cli::Report;
use config::Environment;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::owner::Owner;
use memory_postgres::CaptureOptions;
use memory_postgres::PostgresMemory;
use sqlx::ConnectOptions as _;
use sqlx::PgPool;
use tempfile::TempDir;
use wire::capture::SessionBundle;
use wire::message::Role;

struct Node {
    _home: TempDir,
    directory: TempDir,
    environment: Environment,
}

fn node(extra: &[(&str, &str)]) -> Node {
    let home = support::home();
    let directory = tempfile::tempdir().expect("directory");
    let users = directory.path().join("users.json");
    std::fs::write(
        &users,
        r#"{"ownerUserId":"dana","unmappedIsOwner":true,"users":{}}"#,
    )
    .expect("users file");
    let mut environment = support::environment(&home)
        .with_override("RIVETHUB_USERS_FILE", users.display().to_string())
        .with_override("RIVETHUB_NODE_NAME", "desk");
    for (key, value) in extra {
        environment = environment.with_override(key, value);
    }
    Node {
        _home: home,
        directory,
        environment,
    }
}

async fn run(parts: &[&str], environment: &Environment) -> Report {
    support::run(parts, environment).await
}

fn sqlite_with_sessions(directory: &Path) -> String {
    let path = directory.join("memory.sqlite");
    migrate::sqlite(&path).expect("migrate");
    let connection = rusqlite::Connection::open(&path).expect("sqlite");
    connection
        .execute_batch(
            "INSERT INTO ros_conversations (id, session_key, agent, settings, created_at, updated_at) VALUES
               ('c1', 'claude-code:one', 'rivet', '{\"model\":\"opus\"}', '2026-10-14T09:00:00.000Z', '2026-10-14T10:00:00.000Z'),
               ('c2', 'claude-code:two', 'rivet', '{}', '2026-10-15T09:00:00.000Z', '2026-10-15T10:00:00.000Z'),
               ('c3', 'codex:three', 'rivet-gpt', '{}', '2026-10-16T09:00:00.000Z', '2026-10-16T10:00:00.000Z');
             INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, metadata, created_at) VALUES
               ('m1', 'c1', 'rivet', 'claude-code', 'user', 'first session', '{}', '2026-10-14T09:00:00.000Z'),
               ('m2', 'c2', 'rivet', 'claude-code', 'user', 'second session', '{}', '2026-10-15T09:00:00.000Z'),
               ('m3', 'c3', 'rivet-gpt', 'codex', 'user', 'third session', '{}', '2026-10-16T09:00:00.000Z');",
        )
        .expect("seed");
    path.display().to_string()
}

fn bundle_at(path: &Path) -> SessionBundle {
    capture::bundle::parse(&std::fs::read_to_string(path).expect("bundle file")).expect("bundle")
}

fn session_keys(bundle: &SessionBundle) -> Vec<&str> {
    bundle
        .sessions
        .iter()
        .map(|session| session.origin.session_key.as_str())
        .collect()
}

#[tokio::test]
async fn bundle_takes_every_repeated_session_and_credits_the_node_owner() {
    let node = node(&[]);
    let sqlite = sqlite_with_sessions(node.directory.path());
    let out = node.directory.path().join("s.json");
    let out_text = out.display().to_string();

    let report = run(
        &[
            "memory",
            "bundle",
            "--out",
            &out_text,
            "--session",
            "c3",
            "--session",
            "c1",
            "--sqlite",
            &sqlite,
        ],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(
        report.stdout,
        format!("bundled 2 sessions (raw, not sanitized) to {out_text}\n")
    );
    let bundle = bundle_at(&out);
    assert_eq!(bundle.version, 2);
    assert_eq!(bundle.manifest.session_count, 2);
    assert_eq!(bundle.manifest.exported_by.user, "dana");
    assert_eq!(bundle.manifest.exported_by.node, "desk");
    assert_eq!(session_keys(&bundle), ["claude-code:one", "codex:three"]);
    let first = bundle.sessions.first().expect("first session");
    assert_eq!(first.origin.author, "dana");
    assert_eq!(
        first.batch.settings,
        Some(serde_json::from_str(r#"{"model":"opus"}"#).expect("settings"))
    );
    assert!(!node.directory.path().join("s.json.tmp").exists());

    let everything = run(
        &["memory", "bundle", "--out", &out_text, "--sqlite", &sqlite],
        &node.environment,
    )
    .await;
    assert_eq!(everything.exit, Exit::Ok, "{}", everything.stderr);
    assert_eq!(
        session_keys(&bundle_at(&out)),
        ["claude-code:one", "claude-code:two", "codex:three"]
    );
}

#[tokio::test]
async fn bundle_refuses_an_unknown_session_and_writes_nothing() {
    let node = node(&[]);
    let sqlite = sqlite_with_sessions(node.directory.path());
    let out = node.directory.path().join("s.json");
    let out_text = out.display().to_string();

    let report = run(
        &[
            "memory",
            "bundle",
            "--out",
            &out_text,
            "--session",
            "c1",
            "--session",
            "nope",
            "--sqlite",
            &sqlite,
        ],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Failure);
    assert_eq!(
        report.stderr,
        "Error: no session this node can export has the id nope\n"
    );
    assert!(!out.exists());
    assert!(!node.directory.path().join("s.json.tmp").exists());
}

fn sqlite_with_two_owners(directory: &Path) -> String {
    let path = directory.join("memory.sqlite");
    migrate::sqlite(&path).expect("migrate");
    let connection = rusqlite::Connection::open(&path).expect("sqlite");
    connection
        .execute_batch(
            "INSERT INTO ros_conversations (id, session_key, agent, owner_user_id, settings, created_at, updated_at) VALUES
               ('d1', 'claude-code:dana-old', 'rivet', 'dana', '{}', '2026-10-20T09:00:00.000Z', '2026-10-21T10:00:00.000Z'),
               ('d2', 'claude-code:dana-new', 'rivet', 'dana', '{}', '2026-10-22T09:00:00.000Z', '2026-10-25T10:00:00.000Z'),
               ('p1', 'claude-code:pt-old', 'rivet', 'ben', '{}', '2026-05-11T09:00:00.000Z', '2026-05-12T10:00:00.000Z'),
               ('p2', 'claude-code:pt-new', 'rivet', 'ben', '{}', '2026-08-30T09:00:00.000Z', '2026-09-01T10:00:00.000Z');
             INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, metadata, created_at) VALUES
               ('d1a', 'd1', 'rivet', 'claude-code', 'user', 'a', '{}', '2026-10-20T09:00:00.000Z'),
               ('d2a', 'd2', 'rivet', 'claude-code', 'user', 'a', '{}', '2026-10-22T09:00:00.000Z'),
               ('d2b', 'd2', 'rivet', 'claude-code', 'assistant', 'b', '{}', '2026-10-22T09:01:00.000Z'),
               ('d2c', 'd2', 'rivet', 'claude-code', 'user', 'c', '{}', '2026-10-25T10:00:00.000Z'),
               ('p1a', 'p1', 'rivet', 'claude-code', 'user', 'a', '{}', '2026-05-11T09:00:00.000Z'),
               ('p2a', 'p2', 'rivet', 'claude-code', 'user', 'a', '{}', '2026-08-30T09:00:00.000Z'),
               ('p2b', 'p2', 'rivet', 'claude-code', 'assistant', 'b', '{}', '2026-08-30T09:01:00.000Z'),
               ('p2c', 'p2', 'rivet', 'claude-code', 'user', 'c', '{}', '2026-09-01T10:00:00.000Z');",
        )
        .expect("seed");
    path.display().to_string()
}

async fn attempt(node: &Node, sqlite: &str, selectors: &[&str]) -> Report {
    let out = node.directory.path().join("selected.json");
    let out_text = out.display().to_string();
    std::fs::remove_file(&out).ok();
    let mut parts = vec!["memory", "bundle", "--out", &out_text, "--sqlite", sqlite];
    parts.extend_from_slice(selectors);
    run(&parts, &node.environment).await
}

async fn exported(node: &Node, sqlite: &str, selectors: &[&str]) -> (Report, SessionBundle) {
    let report = attempt(node, sqlite, selectors).await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    let bundle = bundle_at(&node.directory.path().join("selected.json"));
    (report, bundle)
}

async fn refused(node: &Node, sqlite: &str, selectors: &[&str]) -> String {
    let report = attempt(node, sqlite, selectors).await;
    assert_eq!(report.exit, Exit::Failure);
    assert!(!node.directory.path().join("selected.json").exists());
    report.stderr
}

fn authors(bundle: &SessionBundle) -> Vec<(&str, &str)> {
    bundle
        .sessions
        .iter()
        .map(|session| {
            (
                session.origin.session_key.as_str(),
                session.origin.author.as_str(),
            )
        })
        .collect()
}

#[tokio::test]
async fn bundle_defaults_to_the_node_owner_when_the_store_holds_two_owners() {
    let node = node(&[]);
    let sqlite = sqlite_with_two_owners(node.directory.path());

    let (_, bundle) = exported(&node, &sqlite, &[]).await;
    assert_eq!(
        authors(&bundle),
        [
            ("claude-code:dana-old", "dana"),
            ("claude-code:dana-new", "dana")
        ]
    );
}

#[tokio::test]
async fn bundle_owner_selects_another_owner_and_keeps_each_sessions_real_author() {
    let node = node(&[]);
    let sqlite = sqlite_with_two_owners(node.directory.path());

    let (_, bundle) = exported(&node, &sqlite, &["--owner", "ben"]).await;
    assert_eq!(
        authors(&bundle),
        [("claude-code:pt-old", "ben"), ("claude-code:pt-new", "ben")]
    );
    assert_eq!(bundle.manifest.exported_by.user, "dana");

    let (_, bundle) = exported(&node, &sqlite, &["--owner", "ben", "--owner", "dana"]).await;
    assert_eq!(bundle.sessions.len(), 4);
}

#[tokio::test]
async fn bundle_all_owners_takes_every_owner() {
    let node = node(&[]);
    let sqlite = sqlite_with_two_owners(node.directory.path());

    let (_, bundle) = exported(&node, &sqlite, &["--all-owners"]).await;
    assert_eq!(
        authors(&bundle),
        [
            ("claude-code:pt-old", "ben"),
            ("claude-code:pt-new", "ben"),
            ("claude-code:dana-old", "dana"),
            ("claude-code:dana-new", "dana"),
        ]
    );
}

#[tokio::test]
async fn bundle_window_and_limit_count_conversations_by_last_activity() {
    let node = node(&[]);
    let sqlite = sqlite_with_two_owners(node.directory.path());

    let (_, bundle) = exported(
        &node,
        &sqlite,
        &[
            "--all-owners",
            "--since",
            "2026-09-01T00:00:00Z",
            "--until",
            "2026-10-25T00:00:00Z",
        ],
    )
    .await;
    assert_eq!(
        session_keys(&bundle),
        ["claude-code:pt-new", "claude-code:dana-old"]
    );

    let (_, bundle) = exported(&node, &sqlite, &["--all-owners", "--limit", "2"]).await;
    assert_eq!(
        session_keys(&bundle),
        ["claude-code:dana-old", "claude-code:dana-new"],
        "the two most recently active conversations, not the first two message rows"
    );
    assert_eq!(bundle.manifest.session_count, 2);
    assert_eq!(
        bundle
            .sessions
            .iter()
            .map(|session| session.batch.messages.len())
            .collect::<Vec<_>>(),
        [1, 3]
    );
}

#[tokio::test]
async fn bundle_names_the_owner_of_a_session_the_export_does_not_include() {
    let node = node(&[]);
    let sqlite = sqlite_with_two_owners(node.directory.path());

    assert_eq!(
        refused(&node, &sqlite, &["--session", "p2"]).await,
        "Error: session p2 belongs to owner ben, not dana; pass --owner ben or --all-owners\n"
    );

    let (_, bundle) = exported(&node, &sqlite, &["--session", "p2", "--owner", "ben"]).await;
    assert_eq!(session_keys(&bundle), ["claude-code:pt-new"]);

    assert_eq!(
        refused(&node, &sqlite, &["--session", "gone", "--all-owners"]).await,
        "Error: no session this node can export has the id gone\n"
    );
}

#[tokio::test]
async fn bundle_rejects_conflicting_selectors() {
    let node = node(&[]);
    let sqlite = sqlite_with_two_owners(node.directory.path());

    assert_eq!(
        refused(&node, &sqlite, &["--owner", "dana", "--all-owners"]).await,
        "Error: --owner and --all-owners cannot be combined\n"
    );

    assert_eq!(
        refused(&node, &sqlite, &["--session", "d1", "--limit", "1"]).await,
        "Error: --session names sessions directly and cannot be combined with --since, --until or --limit\n"
    );

    assert_eq!(
        refused(&node, &sqlite, &["--until", "yesterday"]).await,
        "Error: invalid --until timestamp: yesterday\n"
    );
}

#[tokio::test]
async fn bundle_dry_run_lists_each_session_with_its_owner_and_writes_nothing() {
    let node = node(&[]);
    let sqlite = sqlite_with_two_owners(node.directory.path());
    let before = std::fs::read(&sqlite).expect("database bytes");

    let report = run(
        &[
            "memory",
            "bundle",
            "--dry-run",
            "--all-owners",
            "--sqlite",
            &sqlite,
        ],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(
        report.stdout,
        "claude-code:pt-old (rivet): 1 message from ben, 2026-05-11 to 2026-05-12\n\
         claude-code:pt-new (rivet): 3 messages from ben, 2026-08-30 to 2026-09-01\n\
         claude-code:dana-old (rivet): 1 message from dana, 2026-10-20 to 2026-10-21\n\
         claude-code:dana-new (rivet): 3 messages from dana, 2026-10-22 to 2026-10-25\n\
         would bundle 4 sessions from ben, dana; nothing written\n"
    );
    assert_eq!(std::fs::read(&sqlite).expect("database bytes"), before);
    assert_eq!(
        listing(node.directory.path()),
        ["memory.sqlite", "users.json"]
    );
}

fn listing(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .expect("listing")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| !name.ends_with("-shm") && !name.ends_with("-wal"))
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn bundle_leaves_a_users_tmp_file_alone_and_no_temporary_behind() {
    let node = node(&[]);
    let sqlite = sqlite_with_sessions(node.directory.path());
    let out = node.directory.path().join("s.json");
    let out_text = out.display().to_string();
    let neighbour = node.directory.path().join("s.json.tmp");
    std::fs::write(&neighbour, "precious user data").expect("neighbour");

    let report = run(
        &["memory", "bundle", "--out", &out_text, "--sqlite", &sqlite],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(
        std::fs::read_to_string(&neighbour).expect("neighbour kept"),
        "precious user data"
    );
    assert_eq!(
        listing(node.directory.path()),
        ["memory.sqlite", "s.json", "s.json.tmp", "users.json"]
    );
}

#[tokio::test]
async fn bundle_refuses_a_directory_out_and_writes_nothing() {
    let node = node(&[]);
    let sqlite = sqlite_with_sessions(node.directory.path());
    let out = node.directory.path().join("outdir");
    std::fs::create_dir(&out).expect("outdir");
    let out_text = out.display().to_string();

    let report = run(
        &["memory", "bundle", "--out", &out_text, "--sqlite", &sqlite],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Failure);
    assert_eq!(
        report.stderr,
        format!("Error: --out {out_text} is a directory; name the bundle file\n")
    );
    assert_eq!(
        listing(node.directory.path()),
        ["memory.sqlite", "outdir", "users.json"]
    );
    assert_eq!(listing(&out), Vec::<String>::new());
}

async fn bundled(node: &Node) -> String {
    let sqlite = sqlite_with_sessions(node.directory.path());
    let out = node.directory.path().join("s.json").display().to_string();
    let report = run(
        &[
            "memory",
            "bundle",
            "--out",
            &out,
            "--session",
            "c1",
            "--session",
            "c3",
            "--sqlite",
            &sqlite,
        ],
        &node.environment,
    )
    .await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    out
}

#[tokio::test]
async fn a_raw_bundle_is_refused_without_force() {
    let node = node(&[]);
    let bundle = bundled(&node).await;

    let report = run(&["memory", "ingest-bundle", &bundle], &node.environment).await;

    assert_eq!(report.exit, Exit::Failure);
    assert_eq!(
        report.stderr,
        format!(
            "Error: {bundle} is a raw bundle that was never sanitized; pass --force to ingest it as it is\n"
        )
    );
}

#[tokio::test]
async fn a_dry_run_lists_every_session_and_its_author() {
    let node = node(&[]);
    let bundle = bundled(&node).await;

    let report = run(
        &["memory", "ingest-bundle", &bundle, "--dry-run"],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(
        report.stdout,
        "claude-code:one (rivet): 1 message from dana, 2026-10-14 to 2026-10-14\n\
         codex:three (rivet-gpt): 1 message from dana, 2026-10-16 to 2026-10-16\n\
         would ingest 2 sessions from dana (raw bundle; ingest needs --force); nothing written\n"
    );
}

#[tokio::test]
async fn bundle_reads_an_explicit_sqlite_file_before_postgres() {
    let node = node(&[(
        "RIVETHUB_PG_URL",
        "postgres://nobody:nothing@127.0.0.1:9/absent",
    )]);
    let sqlite = sqlite_with_sessions(node.directory.path());
    let out = node.directory.path().join("s.json");
    let out_text = out.display().to_string();

    let report = run(
        &["memory", "bundle", "--out", &out_text, "--sqlite", &sqlite],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(bundle_at(&out).manifest.session_count, 3);
}

#[tokio::test]
async fn bundle_requires_a_value_for_every_session_flag() {
    let node = node(&[]);
    for parts in [
        vec!["memory", "bundle", "--out", "s.json", "--session"],
        vec!["memory", "bundle", "--out", "s.json", "--session="],
        vec![
            "memory",
            "bundle",
            "--session",
            "c1",
            "--out",
            "s.json",
            "--session",
            " ",
        ],
    ] {
        let report = run(&parts, &node.environment).await;
        assert_eq!(report.exit, Exit::Failure, "{parts:?}");
        assert_eq!(
            report.stderr, "Error: --session requires a conversation id\n",
            "{parts:?}"
        );
    }
}

fn said(event_id: &str, content: &str) -> CaptureMessage {
    CaptureMessage {
        event_id: event_id.to_owned(),
        role: Role::User,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    }
}

async fn captured(store: &PostgresMemory, owner: &str, session_key: &str) -> String {
    captured_message(store, owner, session_key, 1).await
}

async fn captured_message(
    store: &PostgresMemory,
    owner: &str,
    session_key: &str,
    number: u32,
) -> String {
    let batch = CaptureBatchRequest {
        session_key: session_key.to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("claude-code".to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![said(&format!("{session_key}-{number}"), "hello")],
    };
    let owner = Owner::new(owner.to_owned()).expect("owner");
    store
        .capture_batch(&batch, &CaptureOptions::owned_by(&owner))
        .await
        .expect("capture")
        .conversation_id
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn postgres_bundle_exports_only_the_node_owners_named_sessions(pool: PgPool) {
    let store = PostgresMemory::new(pool.clone());
    let mine = captured(&store, "dana", "claude-code:mine").await;
    let other = captured(&store, "dana", "claude-code:other").await;
    let iness = captured(&store, "ines", "claude-code:ines").await;
    captured_message(&store, "ines", "claude-code:ines", 2).await;
    let url = pool.connect_options().to_url_lossy().to_string();
    let node = node(&[("RIVETHUB_PG_URL", &url)]);
    let out = node.directory.path().join("s.json");
    let out_text = out.display().to_string();

    let named = run(
        &[
            "memory",
            "bundle",
            "--out",
            &out_text,
            "--session",
            &mine,
            "--session",
            &other,
        ],
        &node.environment,
    )
    .await;
    assert_eq!(named.exit, Exit::Ok, "{}", named.stderr);
    let bundle = bundle_at(&out);
    assert_eq!(bundle.manifest.session_count, 2);
    assert!(
        bundle
            .sessions
            .iter()
            .all(|session| session.origin.author == "dana")
    );

    let refused = run(
        &["memory", "bundle", "--out", &out_text, "--session", &iness],
        &node.environment,
    )
    .await;
    assert_eq!(refused.exit, Exit::Failure);
    assert_eq!(
        refused.stderr,
        format!(
            "Error: session {iness} belongs to owner ines, not dana; pass --owner ines or --all-owners\n"
        )
    );
    assert_eq!(bundle_at(&out), bundle);

    let ines = run(
        &["memory", "bundle", "--out", &out_text, "--owner", "ines"],
        &node.environment,
    )
    .await;
    assert_eq!(ines.exit, Exit::Ok, "{}", ines.stderr);
    assert_eq!(authors(&bundle_at(&out)), [("claude-code:ines", "ines")]);

    let newest = run(
        &[
            "memory",
            "bundle",
            "--out",
            &out_text,
            "--all-owners",
            "--limit",
            "1",
        ],
        &node.environment,
    )
    .await;
    assert_eq!(newest.exit, Exit::Ok, "{}", newest.stderr);
    let newest = bundle_at(&out);
    assert_eq!(authors(&newest), [("claude-code:ines", "ines")]);
    assert_eq!(
        newest.sessions.first().map(|s| s.batch.messages.len()),
        Some(2),
        "the limit counts conversations, so the kept one keeps both messages"
    );

    let future = run(
        &[
            "memory",
            "bundle",
            "--out",
            &out_text,
            "--all-owners",
            "--since",
            "2999-01-01T00:00:00Z",
        ],
        &node.environment,
    )
    .await;
    assert_eq!(future.exit, Exit::Ok, "{}", future.stderr);
    assert_eq!(bundle_at(&out).sessions, []);
}

const PLANTED_TOKEN: &str = concat!("ghp_", "Zq8RmT2vLx9WcN4bKp7HyJ3dFs6GtA1eUo5V");

fn planted_bundle(directory: &Path, sanitization: wire::capture::Sanitization) -> String {
    let session = wire::capture::BundledSession {
        origin: wire::capture::SessionOrigin {
            author: "dana".to_owned(),
            node: "desk".to_owned(),
            conversation_id: "c1".to_owned(),
            session_key: "claude-code:one".to_owned(),
            agent: "rivet".to_owned(),
            created_at: None,
            updated_at: None,
            identity: None,
        },
        batch: wire::capture::CaptureBatchRequest {
            session_key: "claude-code:one".to_owned(),
            agent: "rivet".to_owned(),
            channel: None,
            title: None,
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![wire::capture::CaptureMessage {
                event_id: "evt-1".to_owned(),
                role: Role::Assistant,
                content: format!("pushed with {PLANTED_TOKEN} to build.example.com"),
                tool_name: None,
                tool_args: None,
                tool_result: Some("ssh build.example.com: ok".to_owned()),
                metadata: None,
                created_at: None,
            }],
        },
    };
    let sealed = capture::bundle::seal(
        wire::capture::BundleAuthor {
            user: "dana".to_owned(),
            node: "desk".to_owned(),
        },
        Timestamp::UNIX_EPOCH,
        sanitization,
        vec![session],
    )
    .expect("seal");
    let path = directory.join("planted.json");
    std::fs::write(&path, sealed.text).expect("bundle file");
    path.display().to_string()
}

fn mapping(directory: &Path, text: &str) -> String {
    let path = directory.join("redactions.toml");
    std::fs::write(&path, text).expect("mapping file");
    path.display().to_string()
}

#[tokio::test]
async fn a_sanitized_bundle_passes_the_ingest_scan_without_force() {
    let node = node(&[]);
    let raw = planted_bundle(node.directory.path(), wire::capture::Sanitization::Raw);
    let rules = mapping(
        node.directory.path(),
        "[[rule]]\nliteral = \"build.example.com\"\naction = \"map\"\nas = \"<buildhost>\"\n",
    );
    let out = node.directory.path().join("s.clean.json");
    let out_text = out.display().to_string();

    let report = run(
        &[
            "memory",
            "sanitize",
            &raw,
            "--out",
            &out_text,
            "--mapping",
            &rules,
        ],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    let report_path = node.directory.path().join("s.clean.report.md");
    assert_eq!(
        report.stdout,
        format!(
            "sanitized 1 session: 1 secrets, 2 mapped, 0 redacted; report at {}\n\
             review summary: changes by rule: rule at line 1 (map) 2, github_token 1\n\
             not scanned: dates, roles, finalize; numbers, booleans and null stay typed\n\
             the sanitizer is best-effort: it catches known vendor tokens, assignments, name/value pairs, URL credentials, PEM blocks and mapped terms, and can miss unusual code forms, new vendor shapes and bare-word passwords in prose; read the report before you share the bundle\n",
            report_path.display()
        )
    );
    let file = std::fs::read_to_string(&out).expect("sanitized file");
    let markdown = std::fs::read_to_string(&report_path).expect("report file");
    for text in [&file, &markdown] {
        assert!(!text.contains(PLANTED_TOKEN));
        assert!(!text.contains("build.example.com"));
    }
    let sanitized = bundle_at(&out);
    let contents: Vec<&str> = sanitized
        .sessions
        .iter()
        .flat_map(|session| &session.batch.messages)
        .map(|message| message.content.as_str())
        .collect();
    assert_eq!(
        contents,
        ["pushed with [REDACTED:github_token] to <buildhost>"]
    );

    let ingest = run(
        &["memory", "ingest-bundle", &out_text, "--dry-run"],
        &node.environment,
    )
    .await;
    assert_eq!(ingest.exit, Exit::Ok, "{}", ingest.stderr);
    assert!(
        ingest
            .stdout
            .ends_with("would ingest 1 session from dana; nothing written\n"),
        "{}",
        ingest.stdout
    );
}

#[tokio::test]
async fn a_forged_sanitized_stamp_is_held_when_a_secret_still_matches() {
    let node = node(&[]);
    let forged = planted_bundle(
        node.directory.path(),
        wire::capture::Sanitization::Sanitized(wire::capture::SanitizeStamp {
            at: Timestamp::UNIX_EPOCH,
            policy_sha256: "0".repeat(64),
            passes: vec!["secrets".to_owned()],
            unresolved_flags: 0,
            review: None,
        }),
    );

    let held = run(&["memory", "ingest-bundle", &forged], &node.environment).await;

    assert_eq!(held.exit, Exit::Held, "{}", held.stderr);
    assert!(
        held.stdout.starts_with(
            "held: 1 of 1 session has findings (secret \u{d7}1); nothing imported. Decide with --session 1=import|skip|hold-spans\n"
        ),
        "{}",
        held.stdout
    );
    assert!(
        held.stdout.contains("[HELD:github_token]"),
        "{}",
        held.stdout
    );
    assert!(!held.stdout.contains(PLANTED_TOKEN));

    let import = run(
        &["memory", "ingest-bundle", &forged, "--session", "1=import"],
        &node.environment,
    )
    .await;
    assert_eq!(import.exit, Exit::Failure);
    assert_eq!(
        import.stderr,
        format!(
            "Error: {forged}: session 1 has secret findings, which only skip or hold-spans may import; pass --force to import them as they are; nothing was imported\n"
        )
    );

    let spans = run(
        &[
            "memory",
            "ingest-bundle",
            &forged,
            "--all=hold-spans",
            "--dry-run",
        ],
        &node.environment,
    )
    .await;
    assert_eq!(spans.exit, Exit::Ok, "{}", spans.stderr);
    assert!(
        spans
            .stdout
            .ends_with("would ingest 1 session from dana (spans held in 1); nothing written\n"),
        "{}",
        spans.stdout
    );
}

#[tokio::test]
async fn a_mapping_with_a_bad_regex_names_its_line_and_writes_nothing() {
    let node = node(&[]);
    let raw = planted_bundle(node.directory.path(), wire::capture::Sanitization::Raw);
    let rules = mapping(
        node.directory.path(),
        "[[rule]]\nliteral = \"a\"\naction = \"redact\"\n\n[[rule]]\nregex = \"(unclosed\"\naction = \"redact\"\n",
    );
    let out = node.directory.path().join("s.clean.json");

    let report = run(
        &[
            "memory",
            "sanitize",
            &raw,
            "--out",
            &out.display().to_string(),
            "--mapping",
            &rules,
        ],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report.stderr.starts_with(&format!(
            "Error: the mapping file {rules} is invalid: mapping rule at line 5 has a regex that does not compile"
        )),
        "{}",
        report.stderr
    );
    assert_eq!(
        listing(node.directory.path()),
        ["planted.json", "redactions.toml", "users.json"]
    );
}

fn sqlite_with_an_imported_session(directory: &Path) -> String {
    let path = directory.join("memory.sqlite");
    migrate::sqlite(&path).expect("migrate");
    let connection = rusqlite::Connection::open(&path).expect("sqlite");
    connection
        .execute_batch(
            "INSERT INTO ros_conversations (id, session_key, agent, owner_user_id, settings, created_at, updated_at) VALUES
               ('i1', 'import:abc', 'rivet', 'ben', '{\"origin\":{\"author\":\"alice\",\"node\":\"laptop\",\"conversation_id\":\"x1\",\"session_key\":\"claude-code:orig\",\"agent\":\"rivet\",\"created_at\":\"2026-10-01T09:00:00Z\",\"updated_at\":\"2026-10-02T10:00:00Z\"}}', '2026-11-01T09:00:00.000Z', '2026-11-01T10:00:00.000Z');
             INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, metadata, created_at) VALUES
               ('i1a', 'i1', 'rivet', 'claude-code', 'user', 'a', '{}', '2026-11-01T09:00:00.000Z');",
        )
        .expect("seed");
    path.display().to_string()
}

#[tokio::test]
async fn bundle_dry_run_lists_an_imported_session_under_its_local_owner() {
    let node = node(&[]);
    let sqlite = sqlite_with_an_imported_session(node.directory.path());

    let report = run(
        &[
            "memory",
            "bundle",
            "--dry-run",
            "--owner",
            "ben",
            "--sqlite",
            &sqlite,
        ],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(
        report.stdout,
        "claude-code:orig (rivet): 1 message from ben, 2026-10-01 to 2026-10-02\n\
         would bundle 1 session from ben; nothing written\n"
    );

    let (_, bundle) = exported(&node, &sqlite, &["--owner", "ben"]).await;
    assert_eq!(authors(&bundle), [("claude-code:orig", "alice")]);
}
