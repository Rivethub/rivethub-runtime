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
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::message::Role;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const RISKY: &str = "Acme Corp";

const DATES: [&str; 3] = [
    "2026-10-12T09:00:00Z",
    "2026-10-13T10:00:00Z",
    "2026-10-15T09:00:00Z",
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

fn sessions() -> Vec<CaptureBatchRequest> {
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
            vec![said(
                "evt-c1",
                &format!("we paired with {RISKY} on the rollout"),
                "2026-10-15T09:00:00Z",
            )],
        ),
    ]
}

#[derive(serde::Deserialize)]
struct Ask {
    state: String,
}

#[derive(serde::Serialize)]
struct Answer {
    probabilities: [f64; 2],
}

async fn decide(axum::Json(ask): axum::Json<Ask>) -> axum::Json<Answer> {
    let yes = if ask.state.contains(RISKY) {
        0.88
    } else {
        0.05
    };
    axum::Json(Answer {
        probabilities: [1.0 - yes, yes],
    })
}

struct Node {
    home: tempfile::TempDir,
    database: PathBuf,
    owner: kernel::owner::Owner,
    model: String,
}

impl Node {
    async fn new(owner: &str, model: &str, seed: &[CaptureBatchRequest]) -> TestResult<Self> {
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

    fn server(&self, block: bool) -> TestResult<mcp::Server> {
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
        ]);
        let loaded = config::Config::load(
            &env,
            &config::ConfigLocation::Current(state.join("config.yaml")),
            config::Profile::Standalone,
        )?;
        let store = memory_sqlite::SqliteStore::open(&self.database)?;
        let kit = rivethub::sessions::kit(
            &loaded.config,
            &env,
            self.owner.clone(),
            rivethub::sessions::Backend::Sqlite(store.clone()),
        )?;
        Ok(
            mcp::Server::builder(Arc::new(store), false, CancellationToken::new())
                .sessions(kit)
                .build()?,
        )
    }

    fn column(&self, sql: &str) -> TestResult<Vec<String>> {
        let connection = rusqlite::Connection::open(&self.database)?;
        let mut statement = connection.prepare(sql)?;
        let values = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(values)
    }
}

async fn call(server: &mcp::Server, name: &str, arguments: &str) -> TestResult<(bool, String)> {
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

async fn exported(phil: &Node, blocking: &mcp::Server) -> TestResult {
    let (refused, unknown) = call(
        blocking,
        "session_export",
        r#"{"conversation_ids":["00000000-0000-4000-8000-000000000009"],"out":"none.json"}"#,
    )
    .await?;
    assert!(refused, "{unknown}");
    assert_eq!(
        unknown,
        "no session this node can export has the id 00000000-0000-4000-8000-000000000009"
    );
    assert!(!phil.share().join("none.json").exists());

    let ids = phil.column(
        "SELECT id FROM ros_conversations WHERE session_key IN ('claude-code:alpha', 'claude-code:gamma') ORDER BY session_key",
    )?;
    let raw = phil.share().join("work.json");
    let (refused, reply) = call(
        blocking,
        "session_export",
        &format!(
            r#"{{"conversation_ids":["{}"],"out":"work.json"}}"#,
            ids.join(r#"",""#)
        ),
    )
    .await?;
    assert!(!refused, "{reply}");
    assert_eq!(
        reply,
        format!(
            "bundled 2 sessions (raw, not sanitized) to {}\nsanitize it with session_sanitize before it leaves this machine\n",
            path_text(&raw)
        )
    );
    let bundle: wire::capture::SessionBundle =
        serde_json::from_str(&std::fs::read_to_string(&raw)?)?;
    assert_eq!(
        bundle.manifest.sanitization,
        wire::capture::Sanitization::Raw
    );
    assert!(
        bundle
            .sessions
            .iter()
            .all(|session: &BundledSession| session.origin.author == "phil")
    );
    for dry_run in [false, true] {
        let (refused, reply) = call(
            blocking,
            "session_import",
            &format!(r#"{{"path":"work.json","dry_run":{dry_run}}}"#),
        )
        .await?;
        assert!(refused, "{reply}");
        assert!(
            reply.contains("is a raw bundle that was never sanitized"),
            "{reply}"
        );
    }
    Ok(())
}

async fn blocked(phil: &Node, blocking: &mcp::Server) -> TestResult {
    let (refused, reply) = call(
        blocking,
        "session_sanitize",
        r#"{"in":"work.json","out":"work.clean.json","decide":true}"#,
    )
    .await?;
    assert!(!refused, "{reply}");
    assert!(reply.contains("1 flag open (block"), "{reply}");
    assert!(
        reply.contains("flag 1  score 0.88  session 2 message 1 content  report line "),
        "{reply}"
    );
    assert!(!reply.contains(RISKY), "{reply}");
    for name in [
        "work.clean.json",
        "work.clean.report.md",
        "work.clean.review.local.json",
    ] {
        let path = phil.share().join(name);
        assert!(path.is_file(), "{}", path.display());
        assert!(reply.contains(&path_text(&path)), "{reply}");
    }
    let (refused, reply) =
        call(blocking, "session_import", r#"{"path":"work.clean.json"}"#).await?;
    assert!(refused, "{reply}");
    assert!(reply.contains("withholds 1 flagged chunk(s)"), "{reply}");
    let (refused, reply) = call(
        blocking,
        "session_import",
        r#"{"path":"work.clean.json","dry_run":true}"#,
    )
    .await?;
    assert!(refused, "{reply}");
    assert!(
        reply.ends_with("refused: 1 withheld flag open; nothing written\n"),
        "{reply}"
    );

    std::fs::write(phil.share().join("again.review.local.json"), "kept")?;
    let (refused, reply) = call(
        blocking,
        "session_sanitize",
        r#"{"in":"work.json","out":"again.json"}"#,
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

async fn imported(ana: &Node, bundle: &Path) -> TestResult {
    let downloaded = ana.home.path().join("Downloads/advisory.json");
    std::fs::create_dir_all(ana.home.path().join("Downloads"))?;
    std::fs::copy(bundle, &downloaded)?;
    let importer = ana.server(true)?;
    let arguments = |dry_run: bool| {
        format!(
            r#"{{"path":"{}","dry_run":{dry_run}}}"#,
            path_text(&downloaded)
        )
    };
    let (refused, listed) = call(&importer, "session_import", &arguments(true)).await?;
    assert!(!refused, "{listed}");
    assert!(
        listed.ends_with(
            "would import 2 sessions from phil (1 advisory flag unreviewed); nothing written\n"
        ),
        "{listed}"
    );
    assert_eq!(
        ana.column("SELECT id FROM ros_conversations")?,
        Vec::<String>::new()
    );

    let (refused, reply) = call(&importer, "session_import", &arguments(false)).await?;
    assert!(!refused, "{reply}");
    assert_eq!(
        reply,
        "ingested 2 sessions from phil (1 advisory flag unreviewed): 3 inserted, 0 skipped\n"
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

    let (refused, again) = call(&importer, "session_import", &arguments(false)).await?;
    assert!(!refused, "{again}");
    assert_eq!(
        again,
        "ingested 2 sessions from phil (1 advisory flag unreviewed): 0 inserted, 3 skipped\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_bundle_goes_from_one_node_to_another_through_rh_mcp() -> TestResult {
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
    let blocking = phil.server(true)?;
    exported(&phil, &blocking).await?;
    blocked(&phil, &blocking).await?;

    let (refused, reply) = call(
        &phil.server(false)?,
        "session_sanitize",
        r#"{"in":"work.json","out":"advisory.json","decide":true}"#,
    )
    .await?;
    assert!(!refused, "{reply}");
    assert!(reply.contains("1 flag open (advisory"), "{reply}");

    let ana = Node::new("ana", &url, &[]).await?;
    imported(&ana, &phil.share().join("advisory.json")).await?;
    model.close().await?;
    Ok(())
}
