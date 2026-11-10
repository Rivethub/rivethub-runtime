#![cfg(test)]

use kernel::memory::MemoryHitSource;
use kernel::memory::MemorySearchResult;
use kernel::memory::MemoryStore as _;
use kernel::memory::SearchOutcome;

pub mod support;

#[path = "sessions/fixture.rs"]
mod fixture;

use fixture::RISKY;
use fixture::appended;
use fixture::at;
use fixture::blocking;
use fixture::id;
use fixture::node;
use fixture::path_text;
use fixture::sessions;
use fixture::text;
use fixture::unconfigured;

fn hit(key: &str, score: f64, content: &str, created_at: &str) -> MemorySearchResult {
    MemorySearchResult {
        chunk: None,
        id: format!("{key}-{score}"),
        source: MemoryHitSource::Message,
        content: content.to_owned(),
        role: None,
        agent: None,
        relevance_score: score,
        created_at: at(created_at),
        session_id: Some(key.to_owned()),
        title: None,
    }
}

#[tokio::test]
async fn find_groups_hits_by_session() {
    let phil = node("phil", sessions(), unconfigured()).await;
    phil.store
        .set_search_outcome(SearchOutcome {
            results: vec![
                hit(
                    "codex:beta",
                    0.70,
                    "bundle deploy checklist",
                    "2026-10-14T08:00:00Z",
                ),
                hit(
                    "claude-code:alpha",
                    0.91,
                    "migrate the sqlite bundle reader",
                    "2026-10-12T09:00:00Z",
                ),
                hit(
                    "codex:beta",
                    0.69,
                    "bundle   deploy\nnotes",
                    "2026-10-14T08:00:00Z",
                ),
                hit(
                    "claude-code:alpha",
                    0.40,
                    "the bundle reader now reads v1",
                    "2026-10-13T10:00:00Z",
                ),
                hit(
                    "claude-code:gone",
                    0.95,
                    "a session this node cannot export",
                    "2026-10-11T08:00:00Z",
                ),
            ],
            ..SearchOutcome::default()
        })
        .await;

    let result = support::call(&phil.server, "session_find", r#"{"query":"sqlite bundle"}"#).await;

    assert_ne!(result.is_error, Some(true));
    assert_eq!(
        text(&result),
        format!(
            "1. {}  \"SQLite bundle v1 to v2\"  claude-code  2026-10-12 to 2026-10-13  2 messages  best 0.91 (2 hits)\n   > migrate the sqlite bundle reader\n   > the bundle reader now reads v1\n2. {}  \"Deploy notes\"  codex  2026-10-14 to 2026-10-14  1 message  best 0.70 (2 hits)\n   > bundle deploy checklist\n   > bundle deploy notes\nleft out 1 hit from sessions this node cannot export (trashed or not yours)\npass the conversation ids to session_export to write them to a bundle\n",
            id(1),
            id(2)
        )
    );
}

async fn exported_and_sanitized(phil: &fixture::Node) -> std::path::PathBuf {
    let exported = support::call(
        &phil.server,
        "session_export",
        &format!(
            r#"{{"conversation_ids":["{}","{}","{}"],"out":"work.json"}}"#,
            id(1),
            id(3),
            id(1)
        ),
    )
    .await;
    assert_eq!(
        text(&exported),
        format!(
            "bundled 2 sessions (raw, not sanitized) to {}\nsanitize it with session_sanitize before it leaves this machine\n",
            path_text(&phil.share.join("work.json"))
        )
    );
    let sanitized = support::call(
        &phil.server,
        "session_sanitize",
        r#"{"in":"work.json","out":"work.clean.json"}"#,
    )
    .await;
    let reply = text(&sanitized);
    assert_ne!(sanitized.is_error, Some(true), "{reply}");
    let clean = phil.share.join("work.clean.json");
    let report = phil.share.join("work.clean.report.md");
    assert!(
        reply.contains(&format!("bundle: {}\n", path_text(&clean))),
        "{reply}"
    );
    assert!(
        reply.contains(&format!("report: {}\n", path_text(&report))),
        "{reply}"
    );
    assert!(report.is_file());
    clean
}

#[tokio::test]
async fn export_then_sanitize_then_import_round_trips() {
    let phil = node("phil", sessions(), unconfigured()).await;
    let clean = exported_and_sanitized(&phil).await;

    let ana = node("ana", Vec::new(), unconfigured()).await;
    let downloads = ana.home.join("Downloads");
    std::fs::create_dir_all(&downloads).expect("downloads");
    let received = downloads.join("work.clean.json");
    std::fs::copy(&clean, &received).expect("hand over");
    let arguments = |dry_run: bool| {
        format!(
            r#"{{"path":"{}","dry_run":{dry_run}}}"#,
            path_text(&received)
        )
    };

    let listed = support::call(&ana.server, "session_import", &arguments(true)).await;
    assert!(
        text(&listed).ends_with("would import 2 sessions from phil; nothing written\n"),
        "{}",
        text(&listed)
    );
    assert_eq!(appended(&ana.store).await, Vec::new());

    let imported = support::call(&ana.server, "session_import", &arguments(false)).await;
    assert_eq!(
        text(&imported),
        "ingested 2 sessions from phil: 3 inserted, 0 skipped\n"
    );
    let entries = appended(&ana.store).await;
    let mut dates: Vec<_> = entries
        .iter()
        .filter_map(|entry| entry.created_at)
        .collect();
    dates.sort();
    assert_eq!(
        dates,
        [
            at("2026-10-12T09:00:00Z"),
            at("2026-10-13T10:00:00Z"),
            at("2026-10-15T09:00:00Z"),
        ]
    );
    let keys: Vec<String> = entries
        .iter()
        .map(|entry| entry.session_id.clone())
        .collect();
    let origins = ana.store.lookup_origins(keys).await.expect("origins");
    assert_eq!(origins.len(), 2);
    for origin in origins.values() {
        assert_eq!(
            origin
                .deserialize::<wire::capture::SessionOrigin>()
                .expect("origin")
                .author,
            "phil"
        );
    }

    let again = support::call(&ana.server, "session_import", &arguments(false)).await;
    assert_eq!(
        text(&again),
        "ingested 2 sessions from phil: 0 inserted, 3 skipped\n"
    );
}

#[tokio::test]
async fn import_refuses_raw_over_mcp() {
    let phil = node("phil", sessions(), unconfigured()).await;
    support::call(
        &phil.server,
        "session_export",
        &format!(r#"{{"conversation_ids":["{}"],"out":"raw.json"}}"#, id(1)),
    )
    .await;

    for dry_run in [false, true] {
        let result = support::call(
            &phil.server,
            "session_import",
            &format!(r#"{{"path":"raw.json","dry_run":{dry_run}}}"#),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("is a raw bundle that was never sanitized"));
        assert!(!text(&result).contains("claude-code:alpha"));
    }
    assert_eq!(appended(&phil.store).await, Vec::new());
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

#[derive(serde::Deserialize)]
struct Ask {
    state: String,
}

#[derive(serde::Serialize)]
struct Answer {
    probabilities: [f64; 2],
}

#[tokio::test]
async fn import_refuses_open_withheld_flags() {
    let model = testkit::stub_server(
        axum::Router::new()
            .route("/v1/decide", axum::routing::post(decide))
            .route(
                "/v1/decide/info",
                axum::routing::get(|| async { r#"{"model": "stub-model"}"# }),
            ),
    )
    .await
    .expect("stub model");
    let phil = node(
        "phil",
        sessions(),
        Ok(blocking(&format!("http://{}", model.addr))),
    )
    .await;
    support::call(
        &phil.server,
        "session_export",
        &format!(
            r#"{{"conversation_ids":["{}"],"out":"pairing.json"}}"#,
            id(3)
        ),
    )
    .await;
    let sanitized = support::call(
        &phil.server,
        "session_sanitize",
        r#"{"in":"pairing.json","out":"pairing.clean.json","decide":true}"#,
    )
    .await;
    let reply = text(&sanitized);
    assert_ne!(sanitized.is_error, Some(true), "{reply}");
    assert!(reply.contains("1 flag open (block"), "{reply}");
    assert!(
        reply.contains("flag 1  score 0.88  session 1 message 1 content  report line "),
        "{reply}"
    );
    assert!(!reply.contains(RISKY), "{reply}");
    assert!(reply.contains(&path_text(
        &phil.share.join("pairing.clean.review.local.json")
    )));

    let refused = support::call(
        &phil.server,
        "session_import",
        r#"{"path":"pairing.clean.json"}"#,
    )
    .await;
    assert_eq!(refused.is_error, Some(true));
    assert!(
        text(&refused).contains("withholds 1 flagged chunk(s)"),
        "{}",
        text(&refused)
    );
    let listed = support::call(
        &phil.server,
        "session_import",
        r#"{"path":"pairing.clean.json","dry_run":true}"#,
    )
    .await;
    assert_eq!(listed.is_error, Some(true), "{}", text(&listed));
    assert!(
        text(&listed).ends_with("refused: 1 withheld flag open; nothing written\n"),
        "{}",
        text(&listed)
    );
    assert_eq!(appended(&phil.store).await, Vec::new());
    model.close().await.expect("close stub");
}

#[tokio::test]
async fn no_tool_accepts_a_remote_target() {
    let phil = node("phil", sessions(), unconfigured()).await;
    let outside = phil.outside.join("x.json");
    std::fs::write(&outside, "{}").expect("outside file");
    let refused = [
        (
            "session_find",
            r#"{"query":"bundle","gateway":"https://hub.example.com"}"#.to_owned(),
        ),
        (
            "session_export",
            format!(
                r#"{{"conversation_ids":["{}"],"out":"x.json","url":"https://hub.example.com"}}"#,
                id(1)
            ),
        ),
        (
            "session_export",
            format!(
                r#"{{"conversation_ids":["{}"],"out":"https://hub.example.com/x.json"}}"#,
                id(1)
            ),
        ),
        (
            "session_export",
            format!(r#"{{"conversation_ids":["{}"],"out":"../x.json"}}"#, id(1)),
        ),
        (
            "session_export",
            format!(
                r#"{{"conversation_ids":["{}"],"out":"{}"}}"#,
                id(1),
                path_text(&outside)
            ),
        ),
        (
            "session_sanitize",
            r#"{"in":"x.json","out":"y.json","remote":"ssh:hub"}"#.to_owned(),
        ),
        (
            "session_import",
            r#"{"path":"https://hub.example.com/x.json"}"#.to_owned(),
        ),
        ("session_import", r#"{"path":"ssh:hub:x.json"}"#.to_owned()),
        ("session_import", r#"{"path":"../x.json"}"#.to_owned()),
        (
            "session_import",
            format!(r#"{{"path":"{}"}}"#, path_text(&outside)),
        ),
        (
            "session_import",
            r#"{"path":"x.json","target":"hub"}"#.to_owned(),
        ),
    ];
    for (tool, arguments) in &refused {
        let result = support::call(&phil.server, tool, arguments).await;
        assert_eq!(result.is_error, Some(true), "{tool} {arguments}");
    }
    assert!(!phil.share.join("x.json").exists());
    let schemas = serde_json::to_string(
        &phil
            .server
            .tools()
            .iter()
            .filter(|tool| tool.name.starts_with("session_"))
            .map(|tool| tool.input_schema.clone())
            .collect::<Vec<_>>(),
    )
    .expect("schemas");
    let words = ["url", "host", "gateway", "target", "remote"];
    for word in words {
        assert!(
            !schemas.contains(&format!("\"{word}\"")),
            "{word} in {schemas}"
        );
    }
}

#[tokio::test]
async fn export_never_replaces_an_existing_file() {
    let phil = node("phil", sessions(), unconfigured()).await;
    let arguments = format!(r#"{{"conversation_ids":["{}"],"out":"work.json"}}"#, id(1));
    support::call(&phil.server, "session_export", &arguments).await;
    let first = std::fs::read(phil.share.join("work.json")).expect("bundle");

    let again = support::call(&phil.server, "session_export", &arguments).await;

    assert_eq!(again.is_error, Some(true));
    assert!(text(&again).contains("already exists"));
    assert_eq!(
        std::fs::read(phil.share.join("work.json")).expect("bundle"),
        first
    );
}

#[tokio::test]
async fn export_refuses_an_unknown_id_by_name() {
    let phil = node("phil", sessions(), unconfigured()).await;
    let result = support::call(
        &phil.server,
        "session_export",
        &format!(
            r#"{{"conversation_ids":["{}","{}"],"out":"work.json"}}"#,
            id(1),
            id(9)
        ),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        text(&result),
        format!("no session this node can export has the id {}", id(9))
    );
    assert!(!phil.share.join("work.json").exists());
}

#[tokio::test]
async fn reads_refuse_hidden_directories_and_symlink_escapes() {
    let phil = node("phil", sessions(), unconfigured()).await;
    support::call(
        &phil.server,
        "session_export",
        &format!(r#"{{"conversation_ids":["{}"],"out":"work.json"}}"#, id(1)),
    )
    .await;
    let bundle = phil.share.join("work.json");
    let hidden = phil.home.join(".ssh");
    std::fs::create_dir_all(&hidden).expect("hidden dir");
    std::fs::copy(&bundle, hidden.join("work.json")).expect("hidden copy");
    let escaped = phil.outside.join("work.json");
    std::fs::copy(&bundle, &escaped).expect("outside copy");
    std::os::unix::fs::symlink(&escaped, phil.home.join("link.json")).expect("symlink");

    let cases = [
        (hidden.join("work.json"), "is inside a hidden directory"),
        (
            phil.home.join("link.json"),
            "is outside your home directory",
        ),
    ];
    for (path, refusal) in cases {
        let result = support::call(
            &phil.server,
            "session_sanitize",
            &format!(r#"{{"in":"{}","out":"clean.json"}}"#, path_text(&path)),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains(refusal), "{}", text(&result));
    }
    assert!(!phil.share.join("clean.json").exists());
}

#[tokio::test]
async fn session_tools_are_absent_without_a_session_kit() {
    let server = support::server();
    assert!(
        !server
            .tools()
            .iter()
            .any(|tool| tool.name.starts_with("session_"))
    );
    let mut request = rmcp::model::CallToolRequestParams::new("session_find".to_owned());
    request.arguments = Some(serde_json::from_str(r#"{"query":"bundle"}"#).expect("arguments"));
    assert!(server.call(request).await.is_err());
}
