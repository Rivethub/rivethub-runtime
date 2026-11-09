#![cfg(test)]

use crate::support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cli::Exit;
use gateway::tenancy::{Tenancy, UserId};
use memory_postgres::PostgresMemory;
use sqlx::ConnectOptions as _;
use sqlx::PgPool;
use support::{config, environment, home, run};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

const SESSION: &str = "77777777-7777-7777-7777-777777777777";
const NATIVE_KEY: &str = "claude-code:77777777-7777-7777-7777-777777777777";

fn hook_tool(tool_use: &str, listing: &str) -> String {
    format!(
        r#"{{"session_id":"{SESSION}","cwd":"/work/alex","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{{"command":"ls {listing}"}},"tool_use_id":"{tool_use}","tool_response":{{"stdout":"{listing}.txt"}}}}"#
    )
}

fn phils_sender(directory: &Path) -> String {
    let path = directory.join("phil.sqlite");
    migrate::sqlite(&path).expect("migrate");
    let connection = rusqlite::Connection::open(&path).expect("sqlite");
    connection
        .execute_batch(&format!(
            "INSERT INTO ros_conversations (id, session_key, agent, title, settings, created_at, updated_at) VALUES
               ('c1', '{NATIVE_KEY}', 'rivet-claude', 'phil sender title', '{{\"cwd\":\"/work/phil\"}}',
                '2026-10-01T09:00:00.000Z', '2026-10-01T11:00:00.000Z');
             INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, metadata, created_at) VALUES
               ('m1', 'c1', 'rivet-claude', 'claude-code', 'user', 'phil prompt about quokka collisions',
                '{{\"event_id\":\"phil-evt-1\"}}', '2026-10-01T09:00:00.000Z'),
               ('m2', 'c1', 'rivet-claude', 'claude-code', 'assistant', 'phil reply about quokka collisions',
                '{{\"event_id\":\"phil-evt-2\"}}', '2026-10-01T10:00:00.000Z');"
        ))
        .expect("seed");
    path.display().to_string()
}

#[derive(sqlx::FromRow, Debug, PartialEq)]
struct Conversation {
    id: String,
    session_key: String,
    owner_user_id: String,
    title: Option<String>,
    settings: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn conversations(pool: &PgPool) -> Vec<Conversation> {
    sqlx::query_as::<_, Conversation>(
        "select id::text as id, session_key, owner_user_id, title, settings::text as settings, \
         created_at from rh_conversations order by session_key",
    )
    .fetch_all(pool)
    .await
    .expect("conversations")
}

async fn hits(server: &mcp::Server, query: &str) -> Vec<(String, String)> {
    let mut request = rmcp::model::CallToolRequestParams::new("memory_search".to_owned());
    request.arguments = Some(
        serde_json::from_str(&format!(r#"{{"query":"{query}","mode":"trigram"}}"#))
            .expect("arguments"),
    );
    let result = server.call(request).await.expect("search");
    assert_eq!(result.is_error, None, "{result:?}");
    let text = &result
        .content
        .first()
        .expect("content")
        .as_text()
        .expect("text")
        .text;
    text.lines()
        .zip(text.lines().skip(1))
        .filter(|(heading, _)| heading.starts_with("- ["))
        .map(|(heading, body)| (heading.to_owned(), body.to_owned()))
        .collect()
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_import_never_touches_the_receivers_native_session_with_the_same_key(pool: PgPool) {
    let sender_home = home();
    let sender_dir = tempfile::tempdir().expect("sender directory");
    let sender_users = sender_dir.path().join("users.json");
    std::fs::write(
        &sender_users,
        r#"{"ownerUserId":"phil","unmappedIsOwner":true,"users":{}}"#,
    )
    .expect("sender users");
    let sender = environment(&sender_home)
        .with_override("RIVETHUB_USERS_FILE", sender_users.display().to_string())
        .with_override("RIVETHUB_NODE_NAME", "desk");
    let sqlite = phils_sender(sender_dir.path());
    let bundle = sender_dir.path().join("s.json").display().to_string();
    let bundled = run(
        &[
            "memory",
            "bundle",
            "--out",
            &bundle,
            "--session",
            "c1",
            "--sqlite",
            &sqlite,
        ],
        &sender,
    )
    .await;
    assert_eq!(bundled.exit, Exit::Ok, "{}", bundled.stderr);

    let store = Arc::new(PostgresMemory::new(pool.clone()));
    let receiver_home = home();
    let gateway = gateway::Gateway::new(
        Arc::clone(&store) as _,
        crate::capture_hook::settings(receiver_home.path()),
    )
    .with_tenancy(Tenancy::local(
        UserId::new("alex".to_owned()).expect("alex"),
    ));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("listener");
    let port = listener.local_addr().expect("address").port();
    let stop = CancellationToken::new();
    let server = tokio::spawn(gateway::server::serve(
        listener,
        Arc::new(gateway),
        None,
        stop.clone(),
    ));
    config(
        &receiver_home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    let receiver = environment(&receiver_home);
    let now = chrono::Utc::now();
    let live =
        cli::capture_hook::hook(&receiver, &hook_tool("toolu_alex1", "alex-native"), now).await;
    assert_eq!(live.stderr, "");
    let before = conversations(&pool).await;
    let [native_before] = before.as_slice() else {
        panic!("expected one native conversation, found {before:?}");
    };

    let ingested = run(&["memory", "ingest-bundle", &bundle, "--force"], &receiver).await;

    assert_eq!(ingested.exit, Exit::Ok, "{}", ingested.stderr);
    assert_eq!(
        ingested.stdout,
        "ingested 1 session from phil (raw bundle forced): 2 inserted, 0 skipped\n"
    );
    let after = conversations(&pool).await;
    assert_eq!(after.len(), 2, "{after:?}");
    let imported = after
        .iter()
        .find(|row| row.session_key.starts_with("import:") && row.owner_user_id == "alex")
        .unwrap_or_else(|| panic!("no import conversation in {after:?}"));
    assert_eq!(imported.owner_user_id, "alex");
    assert_eq!(imported.title.as_deref(), Some("phil sender title"));
    assert_eq!(
        imported.created_at,
        "2026-10-01T09:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("date")
    );
    assert!(
        after.iter().any(|row| row == native_before),
        "native changed: {native_before:?} -> {after:?}"
    );

    let search = mcp::Server::new(Arc::clone(&store) as _, false, CancellationToken::new())
        .expect("mcp server");
    let untagged = |found: &[(String, String)]| {
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found.iter().all(|(heading, _)| !heading.contains(" from ")),
            "{found:?}"
        );
    };
    let phils = hits(&search, "quokka collisions").await;
    assert_eq!(phils.len(), 2, "{phils:?}");
    for (heading, _) in &phils {
        assert!(heading.ends_with(" from phil · 2026-10-01"), "{phils:?}");
    }
    untagged(&hits(&search, "alex-native").await);

    let again =
        cli::capture_hook::hook(&receiver, &hook_tool("toolu_alex2", "alex-later"), now).await;
    assert_eq!(again.stderr, "");

    let later = conversations(&pool).await;
    assert_eq!(later.len(), 2, "{later:?}");
    assert!(
        later.iter().any(|row| row == imported),
        "import changed: {imported:?} -> {later:?}"
    );
    for (heading, _) in &hits(&search, "quokka collisions").await {
        assert!(heading.ends_with(" from phil · 2026-10-01"), "{heading}");
    }
    untagged(&hits(&search, "alex-later").await);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("gateway stops")
        .expect("gateway task")
        .expect("gateway");
}

#[tokio::test]
async fn a_version_one_bundle_names_no_author() {
    let home = home();
    let directory = tempfile::tempdir().expect("directory");
    let bundle = directory.path().join("v1.json");
    std::fs::write(
        &bundle,
        r#"{"version":1,"sessions":[{"session_key":"claude-code:old","agent":"rivet","channel":"claude-code","messages":[{"event_id":"old-1","role":"user","content":"old work"}]}]}"#,
    )
    .expect("bundle");
    let bundle = bundle.display().to_string();

    let report = run(
        &["memory", "ingest-bundle", &bundle, "--dry-run"],
        &environment(&home),
    )
    .await;

    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(
        report.stdout,
        "claude-code:old (rivet): 1 message from an unknown author, undated\n\
         would ingest 1 session from an unknown author (raw bundle; ingest needs --force); nothing written\n"
    );
}

struct Receiver {
    home: TempDir,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<Result<(), gateway::server::ServeError>>,
}

impl Receiver {
    async fn start(store: &Arc<PostgresMemory>, user: &str) -> Self {
        let home = home();
        let gateway = gateway::Gateway::new(
            Arc::clone(store) as _,
            crate::capture_hook::settings(home.path()),
        )
        .with_tenancy(Tenancy::local(UserId::new(user.to_owned()).expect("user")));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let port = listener.local_addr().expect("address").port();
        let stop = CancellationToken::new();
        let server = tokio::spawn(gateway::server::serve(
            listener,
            Arc::new(gateway),
            None,
            stop.clone(),
        ));
        config(
            &home,
            &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
        );
        Self { home, stop, server }
    }

    async fn ingest(&self, bundle: &str) -> cli::Report {
        run(
            &["memory", "ingest-bundle", bundle, "--force"],
            &environment(&self.home),
        )
        .await
    }

    async fn stop(self) {
        self.stop.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.server)
            .await
            .expect("gateway stops")
            .expect("gateway task")
            .expect("gateway");
    }
}

async fn bundle_from(
    directory: &Path,
    author: &str,
    node: &str,
    conversation: &str,
    text: &str,
) -> String {
    let users = directory.join(format!("{node}-{conversation}-users.json"));
    std::fs::write(
        &users,
        format!(r#"{{"ownerUserId":"{author}","unmappedIsOwner":true,"users":{{}}}}"#),
    )
    .expect("users");
    let path = directory.join(format!("{node}-{conversation}.sqlite"));
    migrate::sqlite(&path).expect("migrate");
    rusqlite::Connection::open(&path)
        .expect("sqlite")
        .execute_batch(&format!(
            "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at) VALUES
               ('{conversation}', 'claude-code:s', 'rivet', '2026-10-01T09:00:00.000Z',
                '2026-10-01T10:00:00.000Z');
             INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, metadata, created_at) VALUES
               ('m-{text}', '{conversation}', 'rivet', 'claude-code', 'user', '{text}',
                '{{\"event_id\":\"evt-{text}\"}}', '2026-10-01T09:00:00.000Z');"
        ))
        .expect("seed");
    let home = home();
    let sender = environment(&home)
        .with_override("RIVETHUB_USERS_FILE", users.display().to_string())
        .with_override("RIVETHUB_NODE_NAME", node);
    let out = directory.join(format!("{text}.json")).display().to_string();
    let bundled = run(
        &[
            "memory",
            "bundle",
            "--out",
            &out,
            "--sqlite",
            &path.display().to_string(),
        ],
        &sender,
    )
    .await;
    assert_eq!(bundled.exit, Exit::Ok, "{}", bundled.stderr);
    out
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn two_users_importing_one_bundle_each_get_a_conversation_of_their_own(pool: PgPool) {
    let directory = tempfile::tempdir().expect("directory");
    let bundle = bundle_from(directory.path(), "phil", "desk", "c1", "wombat-plan").await;
    let store = Arc::new(PostgresMemory::new(pool.clone()));
    let alex = Receiver::start(&store, "alex").await;
    let bob = Receiver::start(&store, "bob").await;

    let by_alex = alex.ingest(&bundle).await;
    let by_bob = bob.ingest(&bundle).await;

    for report in [&by_alex, &by_bob] {
        assert_eq!(
            report.stdout,
            "ingested 1 session from phil (raw bundle forced): 1 inserted, 0 skipped\n",
            "{}",
            report.stderr
        );
    }
    let stored = conversations(&pool).await;
    let mut owners: Vec<&str> = stored
        .iter()
        .map(|row| row.owner_user_id.as_str())
        .collect();
    owners.sort_unstable();
    assert_eq!(owners, ["alex", "bob"], "{stored:?}");
    let message_owners: Vec<(String, String)> = sqlx::query_as(
        "select c.owner_user_id, m.owner_user_id from rh_messages m \
         join rh_conversations c on c.id = m.conversation_id order by 1",
    )
    .fetch_all(&pool)
    .await
    .expect("message owners");
    assert_eq!(
        message_owners,
        [
            ("alex".to_owned(), "alex".to_owned()),
            ("bob".to_owned(), "bob".to_owned())
        ]
    );
    let search = mcp::Server::new(Arc::clone(&store) as _, false, CancellationToken::new())
        .expect("mcp server");
    let found = hits(&search, "wombat-plan").await;
    assert_eq!(found.len(), 2, "{found:?}");
    for (heading, _) in &found {
        assert!(heading.ends_with(" from phil · 2026-10-01"), "{found:?}");
    }
    alex.stop().await;
    bob.stop().await;
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn origins_whose_node_and_conversation_share_a_colon_stay_apart(pool: PgPool) {
    let directory = tempfile::tempdir().expect("directory");
    let phils = bundle_from(directory.path(), "phil", "lab:x", "c", "phil-quokkas").await;
    let alexs = bundle_from(directory.path(), "alex", "lab", "x:c", "alex-quokkas").await;
    let store = Arc::new(PostgresMemory::new(pool.clone()));
    let receiver = Receiver::start(&store, "team").await;

    let first = receiver.ingest(&phils).await;
    let second = receiver.ingest(&alexs).await;

    assert_eq!(first.exit, Exit::Ok, "{}", first.stderr);
    assert_eq!(
        second.stdout, "ingested 1 session from alex (raw bundle forced): 1 inserted, 0 skipped\n",
        "{}",
        second.stderr
    );
    let stored = conversations(&pool).await;
    assert_eq!(stored.len(), 2, "{stored:?}");
    let search = mcp::Server::new(Arc::clone(&store) as _, false, CancellationToken::new())
        .expect("mcp server");
    let phil_hits = hits(&search, "phil-quokkas").await;
    assert!(
        phil_hits
            .iter()
            .all(|(heading, _)| heading.ends_with(" from phil · 2026-10-01")),
        "{phil_hits:?}"
    );
    receiver.stop().await;
}

async fn reexport(pool: &PgPool, directory: &Path, user: &str) -> wire::capture::SessionBundle {
    let (id,): (String,) =
        sqlx::query_as("select id::text from rh_conversations where owner_user_id = $1")
            .bind(user)
            .fetch_one(pool)
            .await
            .expect("conversation");
    let users = directory.join(format!("{user}-users.json"));
    std::fs::write(
        &users,
        format!(r#"{{"ownerUserId":"{user}","unmappedIsOwner":true,"users":{{}}}}"#),
    )
    .expect("users");
    let out = directory.join(format!("{user}-reexport.json"));
    let home = home();
    let exporter = environment(&home)
        .with_override(
            "RIVETHUB_PG_URL",
            pool.connect_options().to_url_lossy().to_string(),
        )
        .with_override("RIVETHUB_USERS_FILE", users.display().to_string())
        .with_override("RIVETHUB_NODE_NAME", format!("{user}-box"));
    let out_text = out.display().to_string();
    let bundled = run(
        &["memory", "bundle", "--out", &out_text, "--session", &id],
        &exporter,
    )
    .await;
    assert_eq!(bundled.exit, Exit::Ok, "{}", bundled.stderr);
    serde_json::from_str(&std::fs::read_to_string(&out).expect("bundle")).expect("parse")
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_session_passed_from_user_to_user_stays_credited_to_its_first_author(pool: PgPool) {
    let directory = tempfile::tempdir().expect("directory");
    let phils = bundle_from(directory.path(), "phil", "desk", "c1", "numbat-notes").await;
    let phils_origin = {
        let bundle: wire::capture::SessionBundle =
            serde_json::from_str(&std::fs::read_to_string(&phils).expect("bundle")).expect("parse");
        bundle.sessions.first().expect("session").origin.clone()
    };
    let store = Arc::new(PostgresMemory::new(pool.clone()));
    let alex = Receiver::start(&store, "alex").await;
    let bob = Receiver::start(&store, "bob").await;
    assert_eq!(alex.ingest(&phils).await.exit, Exit::Ok);

    let alexs = reexport(&pool, directory.path(), "alex").await;
    let path = directory.path().join("from-alex.json");
    std::fs::write(&path, serde_json::to_string(&alexs).expect("json")).expect("write");
    let by_bob = bob.ingest(&path.display().to_string()).await;
    let bobs = reexport(&pool, directory.path(), "bob").await;

    assert_eq!(
        by_bob.stdout, "ingested 1 session from phil (raw bundle forced): 1 inserted, 0 skipped\n",
        "{}",
        by_bob.stderr
    );
    for bundle in [&alexs, &bobs] {
        let session = bundle.sessions.first().expect("session");
        assert_eq!(session.origin, phils_origin);
        assert_eq!(session.batch.session_key, phils_origin.session_key);
        assert_eq!(session.batch.created_at, phils_origin.created_at);
    }
    let search = mcp::Server::new(Arc::clone(&store) as _, false, CancellationToken::new())
        .expect("mcp server");
    let found = hits(&search, "numbat-notes").await;
    assert_eq!(found.len(), 2, "{found:?}");
    for (heading, _) in &found {
        assert!(heading.ends_with(" from phil · 2026-10-01"), "{found:?}");
    }
    alex.stop().await;
    bob.stop().await;
}
