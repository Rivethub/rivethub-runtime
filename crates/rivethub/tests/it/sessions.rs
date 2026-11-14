use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryStore;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResult;
use tokio_util::sync::CancellationToken;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::SessionBundle;
use wire::message::Role;

pub(crate) type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub(crate) const RISKY: &str = "Acme Corp";
const OUTPUT: &str = "payroll.csv: 41 rows, totals 88120";

const DATES: [&str; 4] = [
    "2026-10-12T09:00:00Z",
    "2026-10-13T10:00:00Z",
    "2026-10-15T09:00:00Z",
    "2026-10-15T09:05:00Z",
];

fn said(event_id: &str, content: &str, created_at: &str) -> CaptureMessage {
    CaptureMessage {
        event_id: event_id.to_owned(),
        role: Role::User,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: created_at.parse().ok(),
    }
}

fn ran(event_id: &str, created_at: &str) -> CaptureMessage {
    CaptureMessage {
        role: Role::Assistant,
        tool_name: Some("Bash".to_owned()),
        tool_args: serde_json::from_str(r#"{"command": "cat payroll.csv"}"#).ok(),
        tool_result: Some(OUTPUT.to_owned()),
        ..said(event_id, "reading it", created_at)
    }
}

fn batch(
    key: &str,
    title: &str,
    agent: &str,
    messages: Vec<CaptureMessage>,
) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: key.to_owned(),
        agent: agent.to_owned(),
        channel: None,
        title: Some(title.to_owned()),
        settings: None,
        task_id: None,
        finalize: None,
        created_at: messages.first().and_then(|message| message.created_at),
        updated_at: messages.last().and_then(|message| message.created_at),
        messages,
    }
}

pub(crate) fn sessions() -> Vec<CaptureBatchRequest> {
    vec![
        batch(
            "claude-code:alpha",
            "SQLite bundle v1 to v2",
            "claude-code",
            vec![
                said(
                    "evt-a1",
                    "migrate the sqlite bundle reader",
                    "2026-10-12T09:00:00Z",
                ),
                said(
                    "evt-a2",
                    "the bundle reader now reads v1",
                    "2026-10-13T10:00:00Z",
                ),
            ],
        ),
        batch(
            "codex:beta",
            "Deploy notes",
            "codex",
            vec![said(
                "evt-b1",
                "bundle deploy checklist",
                "2026-10-14T08:00:00Z",
            )],
        ),
        batch(
            "claude-code:gamma",
            "Rollout pairing",
            "claude-code",
            vec![
                said(
                    "evt-c1",
                    &format!("we paired with {RISKY} on the rollout"),
                    "2026-10-15T09:00:00Z",
                ),
                ran("evt-c2", "2026-10-15T09:05:00Z"),
            ],
        ),
        batch(
            "claude-code:delta",
            "Trashed",
            "claude-code",
            vec![said("evt-d1", "thrown away", "2026-10-16T09:00:00Z")],
        ),
    ]
}

#[derive(serde::Deserialize)]
pub(crate) struct Ask {
    state: String,
}

#[derive(serde::Serialize)]
pub(crate) struct Answer {
    probabilities: [f64; 2],
}

pub(crate) async fn decide(axum::Json(ask): axum::Json<Ask>) -> axum::Json<Answer> {
    let yes = if ask.state.contains(RISKY) {
        0.88
    } else {
        0.05
    };
    axum::Json(Answer {
        probabilities: [1.0 - yes, yes],
    })
}

pub(crate) struct Node {
    pub(crate) home: tempfile::TempDir,
    pub(crate) database: PathBuf,
    pub(crate) owner: kernel::owner::Owner,
    model: String,
}

impl Node {
    pub(crate) async fn new(
        owner: &str,
        model: &str,
        seed: &[CaptureBatchRequest],
    ) -> TestResult<Self> {
        let home = tempfile::tempdir()?;
        std::fs::create_dir_all(home.path().join(".rivethub"))?;
        let database = home.path().join(".rivethub/memory.sqlite");
        migrate::sqlite(&database)?;
        let owner = kernel::owner::Owner::new(owner.to_owned())?;
        let store = memory_sqlite::SqliteStore::open(&database)?;
        for batch in seed {
            let batch: kernel::memory::CaptureBatchRequest =
                serde_json::from_value(serde_json::to_value(batch)?)?;
            MemoryStore::capture(&store, batch, &owner, Some("desk")).await?;
        }
        Ok(Self {
            home,
            database,
            owner,
            model: model.to_owned(),
        })
    }

    fn share(&self) -> PathBuf {
        self.home.path().join(".local/share/rivethub/share")
    }

    pub(crate) fn loaded(&self, block: bool) -> TestResult<(config::Config, config::Environment)> {
        let state = self.home.path().join(".rivethub");
        std::fs::write(
            state.join("config.yaml"),
            format!(
                "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n  sanitize:\n    decide:\n      url: {}\n      threshold: 0.3\n      block: {block}\ngateway:\n  host: 127.0.0.1\n  port: 9\n",
                self.database.display(),
                self.model
            ),
        )?;
        let env = config::Environment::from_vars([
            (OsStr::new("HOME"), self.home.path().as_os_str()),
            (OsStr::new("RIVETHUB_NODE_NAME"), OsStr::new("desk")),
            (OsStr::new("RIVETHUB_TOKEN_STORE"), OsStr::new("file")),
        ]);
        let loaded = config::Config::load(
            &env,
            &config::ConfigLocation::Current(state.join("config.yaml")),
        )?;
        Ok((loaded.config, env))
    }

    pub(crate) fn server(&self, block: bool) -> TestResult<mcp::Server> {
        let (config, env) = self.loaded(block)?;
        let store = memory_sqlite::SqliteStore::open(&self.database)?;
        let kit = rivethub::sessions::kit(
            &config,
            &env,
            self.owner.clone(),
            cli::memory::SessionStore::Sqlite(store.clone()),
        )?;
        Ok(
            mcp::Server::builder(Arc::new(store), false, CancellationToken::new())
                .sessions(kit)
                .build()?,
        )
    }

    pub(crate) fn column(&self, sql: &str) -> TestResult<Vec<String>> {
        let connection = rusqlite::Connection::open(&self.database)?;
        let mut statement = connection.prepare(sql)?;
        let values = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(values)
    }
}

pub(crate) async fn call(
    server: &mcp::Server,
    name: &str,
    arguments: &str,
) -> TestResult<(bool, String)> {
    let mut request = CallToolRequestParams::new(name.to_owned());
    request.arguments = Some(serde_json::from_str(arguments)?);
    let result: CallToolResult = server.call(request).await?;
    let text = result
        .content
        .iter()
        .filter_map(|content| content.as_text().map(|text| text.text.clone()))
        .collect();
    Ok((result.is_error == Some(true), text))
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

fn listing(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

pub(crate) fn chosen(phil: &Node) -> TestResult<(String, String, String)> {
    let id = |key: &str| -> TestResult<String> {
        phil.column(&format!(
            "SELECT id FROM ros_conversations WHERE session_key = '{key}'"
        ))?
        .pop()
        .ok_or_else(|| format!("no {key}").into())
    };
    Ok((
        id("claude-code:alpha")?,
        id("claude-code:gamma")?,
        id("claude-code:delta")?,
    ))
}

fn choices(alpha: &str, gamma: &str) -> String {
    format!(
        r#"[{{"id":"{alpha}","decision":"approve"}},{{"id":"{gamma}","decision":"redact","redact":{{"terms":["{RISKY}"],"drop_tool_outputs":true}}}}]"#
    )
}

async fn refused(phil: &Node, server: &mcp::Server, foreign: &str, trashed: &str) -> TestResult {
    let before = listing(&phil.share());
    for id in [foreign, trashed] {
        let (refused, reply) = call(
            server,
            "session_sanitize",
            &format!(r#"{{"sessions":[{{"id":"{id}","decision":"approve"}}],"out":"no.json"}}"#),
        )
        .await?;
        assert!(refused, "{reply}");
        assert_eq!(
            reply,
            format!(
                "no session this node can share has the id {id}; it is unknown, trashed or not the node owner's, and nothing was written"
            )
        );
    }
    assert_eq!(listing(&phil.share()), before);
    Ok(())
}

async fn blocked(phil: &Node, server: &mcp::Server, gamma: &str) -> TestResult {
    let (refused, reply) = call(
        server,
        "session_sanitize",
        &format!(
            r#"{{"sessions":[{{"id":"{gamma}","decision":"approve"}}],"out":"pairing.json","decide":true,"block":true}}"#
        ),
    )
    .await?;
    assert!(!refused, "{reply}");
    assert!(reply.contains("1 flag open (block"), "{reply}");
    assert!(!reply.contains(RISKY), "{reply}");
    let (refused, reply) = call(server, "session_import", r#"{"path":"pairing.json"}"#).await?;
    assert!(refused, "{reply}");
    assert!(reply.contains("withholds 1 flagged chunk(s)"), "{reply}");

    std::fs::write(phil.share().join("again.review.local.json"), "kept")?;
    let (refused, reply) = call(
        server,
        "session_sanitize",
        &format!(r#"{{"sessions":[{{"id":"{gamma}","decision":"approve"}}],"out":"again.json"}}"#),
    )
    .await?;
    assert!(refused, "{reply}");
    assert!(
        reply.contains("again.review.local.json already exists"),
        "{reply}"
    );
    assert!(!phil.share().join("again.json").exists());
    assert!(!phil.share().join("again.report.md").exists());
    Ok(())
}

async fn cli_matches(phil: &Node, server: &mcp::Server, alpha: &str, gamma: &str) -> TestResult {
    let (refused, reply) = call(
        server,
        "session_sanitize",
        &format!(
            r#"{{"sessions":{},"out":"plain.json"}}"#,
            choices(alpha, gamma)
        ),
    )
    .await?;
    assert!(!refused, "{reply}");
    let users = phil.home.path().join("users.json");
    std::fs::write(
        &users,
        r#"{"ownerUserId":"phil","unmappedIsOwner":true,"users":{}}"#,
    )?;
    let env = config::Environment::from_vars([
        (OsStr::new("HOME"), phil.home.path().as_os_str()),
        (OsStr::new("RIVETHUB_NODE_NAME"), OsStr::new("desk")),
        (OsStr::new("RIVETHUB_USERS_FILE"), users.as_os_str()),
    ]);
    let out = phil.home.path().join("cli.json");
    let (redact, database) = (format!("{gamma}={RISKY}"), path_text(&phil.database));
    let args = [
        "rh",
        "memory",
        "sanitize",
        "--session",
        alpha,
        "--session",
        gamma,
        "--redact-term",
        &redact,
        "--drop-tool-outputs",
        gamma,
        "--out",
        &path_text(&out),
        "--sqlite",
        &database,
    ];
    let parsed =
        cli::Cli::parse_args(args.iter().map(Into::into)).map_err(|report| report.stderr)?;
    let cli::Dispatch::Complete(report) =
        cli::dispatch(parsed, &env, &CancellationToken::new()).await
    else {
        return Err("unexpected service start".into());
    };
    assert_eq!(report.exit, cli::Exit::Ok, "{}", report.stderr);
    let read = |path: &Path| -> TestResult<SessionBundle> {
        Ok(capture::bundle::parse(&std::fs::read_to_string(path)?)?)
    };
    assert_eq!(
        read(&out)?.sessions,
        read(&phil.share().join("plain.json"))?.sessions
    );
    Ok(())
}

async fn imported(ana: &Node, bundle: &Path) -> TestResult {
    let downloaded = ana.home.path().join("Downloads/work.clean.json");
    std::fs::create_dir_all(ana.home.path().join("Downloads"))?;
    std::fs::copy(bundle, &downloaded)?;
    let importer = ana.server(true)?;
    let arguments = format!(r#"{{"path":"{}"}}"#, path_text(&downloaded));
    let (refused, reply) = call(&importer, "session_import", &arguments).await?;
    assert!(!refused, "{reply}");
    assert_eq!(
        reply,
        "ingested 2 sessions from phil: 4 inserted, 0 skipped\n"
    );
    let credited = ana.column(
        "SELECT owner_user_id || ' ' || json_extract(settings, '$.origin.author') || '@' || json_extract(settings, '$.origin.node') FROM ros_conversations WHERE session_key LIKE 'import:%'",
    )?;
    assert_eq!(credited, ["ana phil@desk", "ana phil@desk"]);
    let mut dates = ana
        .column("SELECT created_at FROM ros_messages")?
        .iter()
        .map(|text| text.parse())
        .collect::<Result<Vec<DateTime<Utc>>, _>>()?;
    dates.sort();
    let original = DATES
        .iter()
        .map(|text| text.parse())
        .collect::<Result<Vec<DateTime<Utc>>, _>>()?;
    assert_eq!(dates, original);
    Ok(())
}

#[tokio::test]
async fn chosen_sessions_go_from_one_node_to_another_without_a_raw_file() -> TestResult {
    let model = testkit::stub_server(
        axum::Router::new()
            .route("/v1/decide", axum::routing::post(decide))
            .route(
                "/v1/decide/info",
                axum::routing::get(|| async { r#"{"model": "stub-model"}"# }),
            ),
    )
    .await?;
    let url = format!("http://{}", model.addr);
    let phil = Node::new("phil", &url, &sessions()).await?;
    let store = memory_sqlite::SqliteStore::open(&phil.database)?;
    let ana_owner = kernel::owner::Owner::new("ana".to_owned())?;
    let foreign = batch(
        "claude-code:anas",
        "Not phil's",
        "claude-code",
        vec![said("evt-e1", "ana's notes", "2026-10-17T09:00:00Z")],
    );
    MemoryStore::capture(
        &store,
        serde_json::from_value(serde_json::to_value(foreign)?)?,
        &ana_owner,
        Some("desk"),
    )
    .await?;
    rusqlite::Connection::open(&phil.database)?.execute(
        "INSERT INTO rh_session_trash (session_key, trashed_at) VALUES ('claude-code:delta', '2026-10-18T09:00:00.000Z')",
        [],
    )?;
    let (alpha, gamma, trashed) = chosen(&phil)?;
    let foreign = phil
        .column("SELECT id FROM ros_conversations WHERE owner_user_id = 'ana'")?
        .pop()
        .ok_or("ana's session")?;
    let server = phil.server(false)?;
    refused(&phil, &server, &foreign, &trashed).await?;
    assert_eq!(listing(&phil.share()), Vec::<String>::new());

    let (refused, reply) = call(
        &server,
        "session_sanitize",
        &format!(
            r#"{{"sessions":{},"out":"work.clean.json","decide":true}}"#,
            choices(&alpha, &gamma)
        ),
    )
    .await?;
    assert!(!refused, "{reply}");
    assert!(reply.contains("0 flags open (advisory"), "{reply}");
    assert!(
        reply.contains("changes by rule: drop_tool_outputs 1, session_term 1"),
        "{reply}"
    );
    assert!(!reply.contains(RISKY), "{reply}");
    assert_eq!(
        listing(&phil.share()),
        [
            "work.clean.json",
            "work.clean.report.md",
            "work.clean.review.local.json"
        ]
    );
    let bundle = std::fs::read_to_string(phil.share().join("work.clean.json"))?;
    for absent in [RISKY, OUTPUT, "codex:beta", "Deploy notes"] {
        assert!(!bundle.contains(absent), "{absent} in the bundle");
    }
    assert!(bundle.contains("[REDACTED:drop_tool_outputs]"));
    assert!(bundle.contains("cat payroll.csv"));
    let report = std::fs::read_to_string(phil.share().join("work.clean.report.md"))?;
    assert!(report.contains("| session_term |"), "{report}");
    assert!(report.contains("| drop_tool_outputs |"), "{report}");
    assert!(!report.contains(RISKY), "{report}");

    blocked(&phil, &server, &gamma).await?;
    cli_matches(&phil, &server, &alpha, &gamma).await?;
    let ana = Node::new("ana", &url, &[]).await?;
    imported(&ana, &phil.share().join("work.clean.json")).await?;
    model.close().await?;
    Ok(())
}

#[tokio::test]
async fn find_redacts_secrets_in_titles_and_excerpts() -> TestResult {
    let token = concat!("ghp_", "PLANTEDfind8RmT2vLx9WcN4bKp7HyJ3dFs6Gt");
    let seed = [batch(
        "claude-code:delta",
        &format!("rotation of {token}"),
        "claude-code",
        vec![said(
            "evt-d1",
            &format!("token rotation: export GITHUB_TOKEN={token} then rerun"),
            "2026-10-16T09:00:00Z",
        )],
    )];
    let phil = Node::new("phil", "http://127.0.0.1:9", &seed).await?;
    let (refused, reply) = call(
        &phil.server(false)?,
        "session_find",
        r#"{"query":"rotation"}"#,
    )
    .await?;
    assert!(!refused, "{reply}");
    assert!(reply.contains("token rotation"), "{reply}");
    assert!(!reply.contains(token), "{reply}");
    assert!(reply.contains("[REDACTED:"), "{reply}");
    Ok(())
}
