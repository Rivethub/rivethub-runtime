#![cfg(test)]

use crate::support;

use std::path::Path;

use cli::Exit;
use config::Environment;
use tempfile::TempDir;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;

const ONE: &str = "00000000-0000-4000-8000-000000000001";
const TWO: &str = "00000000-0000-4000-8000-000000000002";
const TRASHED: &str = "00000000-0000-4000-8000-000000000003";
const ANAS: &str = "00000000-0000-4000-8000-000000000004";
const CLIENT: &str = "Falcon Labs";
const OUTPUT: &str = "payroll.csv: 41 rows";

struct Node {
    _home: TempDir,
    directory: TempDir,
    environment: Environment,
    sqlite: String,
}

fn node() -> Node {
    let home = support::home();
    let directory = tempfile::tempdir().expect("directory");
    let users = directory.path().join("users.json");
    std::fs::write(
        &users,
        r#"{"ownerUserId":"phil","unmappedIsOwner":true,"users":{}}"#,
    )
    .expect("users file");
    let environment = support::environment(&home)
        .with_override("RIVETHUB_USERS_FILE", users.display().to_string())
        .with_override("RIVETHUB_NODE_NAME", "desk");
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path).expect("migrate");
    rusqlite::Connection::open(&path)
        .expect("sqlite")
        .execute_batch(&format!(
            "INSERT INTO ros_conversations (id, session_key, agent, owner_user_id, created_at, updated_at) VALUES
               ('{ONE}', 'claude-code:one', 'rivet', 'phil', '2026-10-14T09:00:00.000Z', '2026-10-14T10:00:00.000Z'),
               ('{TWO}', 'claude-code:two', 'rivet', 'phil', '2026-10-15T09:00:00.000Z', '2026-10-15T10:00:00.000Z'),
               ('{TRASHED}', 'claude-code:gone', 'rivet', 'phil', '2026-10-16T09:00:00.000Z', '2026-10-16T10:00:00.000Z'),
               ('{ANAS}', 'claude-code:ana', 'rivet', 'ana', '2026-10-17T09:00:00.000Z', '2026-10-17T10:00:00.000Z');
             INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, tool_name, tool_args, tool_result, metadata, created_at) VALUES
               ('m1', '{ONE}', 'rivet', 'claude-code', 'user', 'first session with {CLIENT}', NULL, NULL, NULL, '{{}}', '2026-10-14T09:00:00.000Z'),
               ('m2', '{TWO}', 'rivet', 'claude-code', 'user', 'second session with {CLIENT}', NULL, NULL, NULL, '{{}}', '2026-10-15T09:00:00.000Z'),
               ('m3', '{TWO}', 'rivet', 'claude-code', 'tool', '', 'Bash', '{{\"command\":\"cat payroll.csv\"}}', '{OUTPUT}', '{{}}', '2026-10-15T09:01:00.000Z'),
               ('m4', '{TRASHED}', 'rivet', 'claude-code', 'user', 'trashed', NULL, NULL, NULL, '{{}}', '2026-10-16T09:00:00.000Z'),
               ('m5', '{ANAS}', 'rivet', 'claude-code', 'user', 'not mine', NULL, NULL, NULL, '{{}}', '2026-10-17T09:00:00.000Z');
             INSERT INTO rh_session_trash (session_key, trashed_at) VALUES ('claude-code:gone', '2026-10-18T09:00:00.000Z');"
        ))
        .expect("seed");
    Node {
        _home: home,
        sqlite: path.display().to_string(),
        directory,
        environment,
    }
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
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn sanitize_reads_chosen_sessions_from_the_store_and_writes_no_raw_bundle() {
    let node = node();
    let share = node.directory.path().join("share");
    std::fs::create_dir(&share).expect("share");
    let out = share.join("work.clean.json");
    let redact = format!("{TWO}={CLIENT}");

    let report = support::run(
        &[
            "memory",
            "sanitize",
            "--session",
            ONE,
            "--session",
            TWO,
            "--redact-term",
            &redact,
            "--drop-tool-outputs",
            TWO,
            "--out",
            &out.display().to_string(),
            "--sqlite",
            &node.sqlite,
        ],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(
        report
            .stdout
            .contains("changes by rule: drop_tool_outputs 1, session_term 1\n"),
        "{}",
        report.stdout
    );
    assert_eq!(listing(&share), ["work.clean.json", "work.clean.report.md"]);
    let text = std::fs::read_to_string(&out).expect("bundle");
    let bundle: SessionBundle = capture::bundle::parse(&text).expect("parse");
    assert!(matches!(
        bundle.manifest.sanitization,
        Sanitization::Sanitized(_)
    ));
    let contents: Vec<&str> = bundle
        .sessions
        .iter()
        .flat_map(|session| &session.batch.messages)
        .map(|message| message.content.as_str())
        .collect();
    assert_eq!(
        contents,
        [
            "first session with Falcon Labs",
            "second session with [REDACTED:session_term]",
            ""
        ]
    );
    assert!(!text.contains(OUTPUT));
    assert!(text.contains("[REDACTED:drop_tool_outputs]"));
    let report = std::fs::read_to_string(share.join("work.clean.report.md")).expect("report");
    assert!(!report.contains(CLIENT));
}

#[tokio::test]
async fn sanitize_refuses_a_trashed_or_foreign_session_by_name() {
    let node = node();
    for refused in [TRASHED, ANAS] {
        let out = node.directory.path().join("refused.json");
        let report = support::run(
            &[
                "memory",
                "sanitize",
                "--session",
                ONE,
                "--session",
                refused,
                "--out",
                &out.display().to_string(),
                "--sqlite",
                &node.sqlite,
            ],
            &node.environment,
        )
        .await;
        assert_eq!(report.exit, Exit::Failure);
        assert_eq!(
            report.stderr,
            format!(
                "Error: no session this node can share has the id {refused}; it is unknown, trashed or not the node owner's, and nothing was written\n"
            )
        );
        assert_eq!(
            listing(node.directory.path()),
            ["memory.sqlite", "users.json"]
        );
    }
}
