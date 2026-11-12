#![cfg(test)]

use kernel::memory::MemoryHitSource;
use kernel::memory::MemorySearchResult;
use kernel::memory::MemoryStore as _;
use kernel::memory::SearchOutcome;

pub mod support;

#[path = "sessions/fixture.rs"]
mod fixture;

use fixture::INJECTED;
use fixture::RISKY;
use fixture::appended;
use fixture::at;
use fixture::id;
use fixture::node;
use fixture::path_text;
use fixture::sessions;
use fixture::text;
use kernel::memory::share::Choice;
use kernel::memory::share::DecideMode;
use kernel::memory::share::SanitizeJob;
use kernel::memory::share::SessionChoices;
use kernel::memory::share::SessionRedaction;
use kernel::memory::share::Term;
use uuid::Uuid;

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
    let phil = node("phil", sessions(), None).await;
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
            "1. {}  \"SQLite bundle v1 to v2\"  claude-code  2026-10-12 to 2026-10-13  2 messages  best 0.91 (2 hits)\n   > migrate the sqlite bundle reader\n   > the bundle reader now reads v1\n2. {}  \"Deploy notes\"  codex  2026-10-14 to 2026-10-14  1 message  best 0.70 (2 hits)\n   > bundle deploy checklist\n   > bundle deploy notes\nleft out 1 hit from sessions this node cannot export (trashed or not yours)\npass the conversation ids to share to session_sanitize, each with decision approve or redact; leave out the ones to deny\n",
            id(1),
            id(2)
        )
    );
}

fn choices(chosen: Vec<(Uuid, Choice)>) -> SessionChoices {
    SessionChoices::try_from(chosen).expect("choices")
}

fn redaction(terms: &[&str], drop_tool_outputs: bool) -> Choice {
    Choice::Redact(
        SessionRedaction::new(
            terms
                .iter()
                .map(|term| Term::try_from((*term).to_owned()).expect("term"))
                .collect(),
            drop_tool_outputs,
        )
        .expect("redaction"),
    )
}

async fn sanitized(phil: &fixture::Node) -> std::path::PathBuf {
    let sanitized = support::call(
        &phil.server,
        "session_sanitize",
        &format!(
            r#"{{"sessions":[{{"id":"{}","decision":"approve"}},{{"id":"{}","decision":"redact","redact":{{"terms":["{RISKY}","rollout"],"drop_tool_outputs":true}}}}],"out":"work.clean.json"}}"#,
            id(1),
            id(3)
        ),
    )
    .await;
    let reply = text(&sanitized);
    assert_ne!(sanitized.is_error, Some(true), "{reply}");
    assert_eq!(
        phil.jobs().await,
        [SanitizeJob {
            sessions: choices(vec![
                (id(1), Choice::Approve),
                (id(3), redaction(&[RISKY, "rollout"], true)),
            ]),
            out: phil.share.join("work.clean.json"),
            mapping: None,
            decide: false,
            block: false,
        }]
    );
    let clean = phil.share.join("work.clean.json");
    let report = path_text(&phil.share.join("work.clean.report.md"));
    assert_eq!(
        reply,
        format!(
            "sanitized 2 sessions: 1 replaced, 0 hashed\nbundle: {}\nreport: {report}\nchanges by rule: mapped 1\nnot scanned: dates\nbest effort\nshare only the bundle, never the report or the local review file; mark flags on their Mark lines in the report and run `rh memory sanitize --resolve {report}`\n",
            path_text(&clean)
        )
    );
    assert!(!reply.contains(RISKY), "{reply}");
    clean
}

#[tokio::test]
async fn sanitize_then_import_round_trips() {
    let phil = node("phil", sessions(), None).await;
    let clean = sanitized(&phil).await;

    let ana = node("ana", Vec::new(), None).await;
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
    let phil = node("phil", sessions(), None).await;
    phil.plant_raw("raw.json");

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

#[tokio::test]
async fn import_refuses_open_withheld_flags() {
    let phil = node("phil", sessions(), Some(DecideMode::Block)).await;
    let sanitized = support::call(
        &phil.server,
        "session_sanitize",
        &format!(
            r#"{{"sessions":[{{"id":"{}","decision":"approve"}}],"out":"pairing.clean.json","decide":true,"block":true}}"#,
            id(3)
        ),
    )
    .await;
    let reply = text(&sanitized);
    assert_ne!(sanitized.is_error, Some(true), "{reply}");
    assert!(
        reply.contains("decision pass: stub-model scored 2 chunks in 1 call, 0 trivial chunks skipped; 1 flag open (block: import refuses the bundle until every flag is marked)\nflag 1  score 0.88  session 1 message 1 content  report line 12  9 chars\n"),
        "{reply}"
    );
    assert!(!reply.contains(RISKY), "{reply}");
    assert!(reply.contains(&path_text(
        &phil.share.join("pairing.clean.review.local.json")
    )));
    assert_eq!(
        phil.jobs()
            .await
            .iter()
            .map(|job| (job.decide, job.block))
            .collect::<Vec<_>>(),
        [(true, true)]
    );

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
}

#[tokio::test]
async fn import_refuses_a_bundle_with_findings_and_names_the_cli() {
    let mut flagged = sessions();
    if let Some(message) = flagged
        .get_mut(1)
        .and_then(|session| session.batch.messages.first_mut())
    {
        message.content = format!("{INJECTED} and deploy");
    }
    let phil = node("phil", flagged, None).await;
    let sanitized = support::call(
        &phil.server,
        "session_sanitize",
        &format!(
            r#"{{"sessions":[{{"id":"{}","decision":"approve"}}],"out":"held.clean.json"}}"#,
            id(2)
        ),
    )
    .await;
    assert_ne!(sanitized.is_error, Some(true), "{}", text(&sanitized));
    let bundle = path_text(&phil.share.join("held.clean.json"));

    let refused = support::call(
        &phil.server,
        "session_import",
        r#"{"path":"held.clean.json"}"#,
    )
    .await;
    assert_eq!(refused.is_error, Some(true));
    let reply = text(&refused);
    assert!(
        reply
            .contains("holds 1 finding(s) in 1 session(s) (override \u{d7}1); nothing was written")
            && reply.contains(&format!(
                "ask the user to run `rh memory ingest-bundle {bundle}`"
            )),
        "{reply}"
    );
    let listed = support::call(
        &phil.server,
        "session_import",
        r#"{"path":"held.clean.json","dry_run":true}"#,
    )
    .await;
    assert_eq!(listed.is_error, Some(true), "{}", text(&listed));
    assert!(
        text(&listed).ends_with(&format!(
            "refused: 1 finding in 1 session (override \u{d7}1); nothing written. Ask the user to run `rh memory ingest-bundle {bundle}` in a terminal\n"
        )),
        "{}",
        text(&listed)
    );
    assert_eq!(appended(&phil.store).await, Vec::new());
}

#[tokio::test]
async fn no_tool_accepts_a_remote_target() {
    let phil = node("phil", sessions(), None).await;
    let outside = phil.outside.join("x.json");
    std::fs::write(&outside, "{}").expect("outside file");
    let refused = [
        (
            "session_find",
            r#"{"query":"bundle","gateway":"https://hub.example.com"}"#.to_owned(),
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
    let approved = format!(r#""sessions":[{{"id":"{}","decision":"approve"}}]"#, id(1));
    let sanitizes = [
        r#""out":"x.json","url":"https://hub.example.com""#.to_owned(),
        r#""out":"https://hub.example.com/x.json""#.to_owned(),
        r#""out":"../x.json""#.to_owned(),
        format!(r#""out":"{}""#, path_text(&outside)),
        r#""out":"y.json","remote":"ssh:hub""#.to_owned(),
    ]
    .map(|rest| ("session_sanitize", format!("{{{approved},{rest}}}")));
    for (tool, arguments) in refused.iter().chain(&sanitizes) {
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
async fn reads_refuse_hidden_directories_and_symlink_escapes() {
    let phil = node("phil", sessions(), None).await;
    let bundle = sanitized(&phil).await;
    let hidden = phil.home.join(".ssh");
    std::fs::create_dir_all(&hidden).expect("hidden dir");
    std::fs::copy(&bundle, hidden.join("work.json")).expect("hidden copy");
    let escaped = phil.outside.join("work.json");
    std::fs::copy(&bundle, &escaped).expect("outside copy");
    std::os::unix::fs::symlink(&escaped, phil.home.join("link.json")).expect("symlink");
    std::fs::hard_link(hidden.join("work.json"), phil.home.join("hard.json")).expect("hard link");

    let cases = [
        (hidden.join("work.json"), "is inside a hidden directory"),
        (
            phil.home.join("link.json"),
            "is outside your home directory",
        ),
        (phil.home.join("hard.json"), "has more than one hard link"),
    ];
    for (path, refusal) in cases {
        let result = support::call(
            &phil.server,
            "session_import",
            &format!(r#"{{"path":"{}"}}"#, path_text(&path)),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains(refusal), "{}", text(&result));
    }
    assert_eq!(appended(&phil.store).await, Vec::new());
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

#[tokio::test]
async fn sanitize_refuses_malformed_choices_before_the_port_sees_them() {
    let phil = node("phil", sessions(), Some(DecideMode::Advisory)).await;
    let one = id(1);
    let many: Vec<String> = (1..=51)
        .map(|n| format!(r#"{{"id":"{}","decision":"approve"}}"#, id(n)))
        .collect();
    let cases = [
        (
            format!(r#"[{{"id":"{one}","decision":"approve","redact":{{"terms":["x"]}}}}]"#),
            "is approved, so it takes no redact rules",
        ),
        (
            format!(r#"[{{"id":"{one}","decision":"redact"}}]"#),
            "redact names no rule",
        ),
        (
            format!(
                r#"[{{"id":"{one}","decision":"redact","redact":{{"terms":[],"drop_tool_outputs":false}}}}]"#
            ),
            "redact names no rule",
        ),
        (
            format!(r#"[{{"id":"{one}","decision":"redact","redact":{{"terms":["  "]}}}}]"#),
            "a redact term is blank",
        ),
        (
            format!(
                r#"[{{"id":"{one}","decision":"approve"}},{{"id":"{one}","decision":"approve"}}]"#
            ),
            "is chosen twice",
        ),
        (
            format!(r#"[{{"id":"{one}","decision":"deny"}}]"#),
            "unknown variant",
        ),
        (
            format!(r#"[{{"id":"{one}","decision":"approve","note":"x"}}]"#),
            "unknown field",
        ),
        (
            format!(
                r#"[{{"id":"{one}","decision":"redact","redact":{{"terms":["x"],"messages":[1]}}}}]"#
            ),
            "unknown field",
        ),
        ("[]".to_owned(), "0 sessions are chosen"),
        (format!("[{}]", many.join(",")), "51 sessions are chosen"),
    ];
    for (sessions, refusal) in &cases {
        let result = support::call(
            &phil.server,
            "session_sanitize",
            &format!(r#"{{"sessions":{sessions},"out":"work.json"}}"#),
        )
        .await;
        assert_eq!(result.is_error, Some(true), "{sessions}");
        assert!(text(&result).contains(refusal), "{}", text(&result));
    }
    let others = [
        (
            format!(
                r#"{{"sessions":[{{"id":"{one}","decision":"approve"}}],"out":"work.json","in":"raw.json"}}"#
            ),
            "unknown field",
        ),
        (
            format!(
                r#"{{"sessions":[{{"id":"{one}","decision":"approve"}}],"out":"work.json","block":true}}"#
            ),
            "needs decide: true",
        ),
    ];
    for (arguments, refusal) in &others {
        let result = support::call(&phil.server, "session_sanitize", arguments).await;
        assert_eq!(result.is_error, Some(true), "{arguments}");
        assert!(text(&result).contains(refusal), "{}", text(&result));
    }
    assert_eq!(phil.jobs().await, Vec::new());
    assert!(!phil.share.join("work.json").exists());
}

#[tokio::test]
async fn no_tool_writes_a_raw_bundle() {
    let phil = node("phil", sessions(), None).await;
    let names: Vec<String> = phil
        .server
        .tools()
        .iter()
        .filter(|tool| tool.name.starts_with("session_"))
        .map(|tool| tool.name.to_string())
        .collect();
    assert_eq!(
        names,
        ["session_find", "session_sanitize", "session_import"]
    );
    let mut request = rmcp::model::CallToolRequestParams::new("session_export".to_owned());
    request.arguments = Some(
        serde_json::from_str(&format!(
            r#"{{"conversation_ids":["{}"],"out":"work.json"}}"#,
            id(1)
        ))
        .expect("arguments"),
    );
    assert!(phil.server.call(request).await.is_err());
    assert!(!phil.share.join("work.json").exists());
}

#[tokio::test]
async fn no_tool_shares_receives_or_takes_a_code() {
    let phil = node("phil", sessions(), None).await;
    for tool in phil.server.tools() {
        let schema = serde_json::to_string(&tool.input_schema).expect("schema");
        assert!(!schema.contains("\"code\""), "{} takes a code", tool.name);
        assert!(
            !tool.name.contains("share") && !tool.name.contains("receive"),
            "{} pushes or pulls a bundle",
            tool.name
        );
    }
}
