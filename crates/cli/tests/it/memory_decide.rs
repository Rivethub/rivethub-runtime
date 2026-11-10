#![cfg(test)]

use crate::support;

use std::path::Path;

use cli::Exit;
use cli::Report;
use config::Environment;
use tempfile::TempDir;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;
use wire::message::Role;

const RISKY: &str = "Acme Corp";

#[derive(serde::Deserialize)]
struct Ask {
    state: String,
}

#[derive(serde::Serialize)]
struct Answer {
    probabilities: [f64; 2],
}

struct Node {
    home: TempDir,
    directory: TempDir,
    environment: Environment,
}

const SUMMARY_TAIL: &str = "not scanned: dates, roles, finalize; numbers, booleans and null stay typed\nthe sanitizer is best-effort: it catches known vendor tokens, assignments, name/value pairs, URL credentials, PEM blocks and mapped terms, and can miss unusual code forms, new vendor shapes and bare-word passwords in prose; read the report before you share the bundle\n";

fn node(decide_url: Option<&str>) -> Node {
    let home = support::home();
    let directory = tempfile::tempdir().expect("directory");
    let users = directory.path().join("users.json");
    std::fs::write(
        &users,
        r#"{"ownerUserId":"phil","unmappedIsOwner":true,"users":{}}"#,
    )
    .expect("users file");
    let decide = decide_url.map_or_else(String::new, |url| {
        format!("memory:\n  sanitize:\n    decide:\n      url: {url}\n      top: 5\n")
    });
    support::config(&home, &decide);
    let environment = support::environment(&home)
        .with_override("RIVETHUB_USERS_FILE", users.display().to_string())
        .with_override("RIVETHUB_NODE_NAME", "desk");
    Node {
        home,
        directory,
        environment,
    }
}

async fn run(parts: &[&str], environment: &Environment) -> Report {
    support::run(parts, environment).await
}

async fn decide(axum::Json(ask): axum::Json<Ask>) -> axum::Json<Answer> {
    let state = ask.state;
    let yes = if state.contains(RISKY) { 0.88 } else { 0.05 };
    axum::Json(Answer {
        probabilities: [1.0 - yes, yes],
    })
}

async fn model() -> testkit::Stub {
    let routes = axum::Router::new()
        .route("/v1/decide", axum::routing::post(decide))
        .route(
            "/v1/decide/info",
            axum::routing::get(|| async { r#"{"model": "stub-model"}"# }),
        );
    testkit::stub_server(routes).await.expect("stub model")
}

fn raw_bundle(directory: &Path) -> String {
    let message = |event_id: &str, content: &str| wire::capture::CaptureMessage {
        event_id: event_id.to_owned(),
        role: Role::Assistant,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    };
    let session = wire::capture::BundledSession {
        origin: wire::capture::SessionOrigin {
            author: "phil".to_owned(),
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
            messages: vec![
                message("evt-1", "Added a parser test; all 42 passed."),
                message("evt-2", &format!("{RISKY} is churning next quarter.")),
            ],
        },
    };
    let sealed = capture::bundle::seal(
        wire::capture::BundleAuthor {
            user: "phil".to_owned(),
            node: "desk".to_owned(),
        },
        chrono::DateTime::UNIX_EPOCH,
        Sanitization::Raw,
        vec![session],
    )
    .expect("seal");
    let path = directory.join("s.json");
    std::fs::write(&path, sealed.text).expect("bundle file");
    path.display().to_string()
}

fn bundle_at(path: &Path) -> SessionBundle {
    capture::bundle::parse(&std::fs::read_to_string(path).expect("bundle file")).expect("bundle")
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

struct Decided {
    out: String,
    report: String,
    rules: String,
}

async fn sanitize_with_decide(node: &Node, block: bool) -> Decided {
    let directory = node.directory.path();
    let raw = raw_bundle(directory);
    let decided = Decided {
        out: directory.join("s.clean.json").display().to_string(),
        report: directory.join("s.clean.report.md").display().to_string(),
        rules: directory.join("redactions.toml").display().to_string(),
    };
    std::fs::write(&decided.rules, "").expect("empty mapping");
    let mut args = vec![
        "memory",
        "sanitize",
        &raw,
        "--out",
        &decided.out,
        "--mapping",
        &decided.rules,
        "--decide",
    ];
    if block {
        args.push("--block");
    }
    let sanitize = run(&args, &node.environment).await;
    assert_eq!(sanitize.exit, Exit::Ok, "{}", sanitize.stderr);
    let next = if block {
        format!(
            "mark them in {} (block: the bundle withholds them until you do)",
            decided.report
        )
    } else {
        format!(
            "review them in {} (advisory: ingest does not wait)",
            decided.report
        )
    };
    assert_eq!(
        sanitize.stdout,
        format!(
            "sanitized 1 session: 0 secrets, 0 mapped, 0 redacted; report at {}\ndecision pass: stub-model scored 2 chunks in 2 calls, 0 trivial chunks skipped; 1 flag for review (top 5 per session at or over 0.30); {next}\nreview summary: changes by rule: no rule matched\n{SUMMARY_TAIL}",
            decided.report
        )
    );
    decided
}

#[tokio::test]
async fn an_advisory_flag_stays_in_the_bundle_and_ingest_goes_ahead() {
    let stub = model().await;
    let node = node(Some(&format!("http://{}", stub.addr)));
    let decided = sanitize_with_decide(&node, false).await;

    assert!(
        std::fs::read_to_string(&decided.out)
            .expect("bundle")
            .contains(RISKY)
    );
    assert!(
        std::fs::read_to_string(&decided.report)
            .expect("report")
            .contains(&format!("Mark: open\n\n~~~text\n{RISKY} is churning"))
    );
    let ingest = run(
        &["memory", "ingest-bundle", &decided.out, "--dry-run"],
        &node.environment,
    )
    .await;
    assert_eq!(ingest.exit, Exit::Ok, "{}", ingest.stderr);
    assert!(
        ingest.stdout.ends_with(
            "would ingest 1 session from phil (1 advisory flags unreviewed); nothing written\n"
        ),
        "{}",
        ingest.stdout
    );
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn a_blocking_flag_stays_out_of_the_bundle_and_blocks_ingest() {
    let stub = model().await;
    let node = node(Some(&format!("http://{}", stub.addr)));
    let decided = sanitize_with_decide(&node, true).await;

    assert!(
        !std::fs::read_to_string(&decided.out)
            .expect("bundle")
            .contains(RISKY)
    );
    let refused = run(
        &["memory", "ingest-bundle", &decided.out],
        &node.environment,
    )
    .await;
    assert_eq!(refused.exit, Exit::Failure);
    assert_eq!(
        refused.stderr,
        format!(
            "Error: {} withholds 1 flagged chunk(s) until you mark them; mark them in the report and run rh memory sanitize --resolve <report>, or pass --force to ingest it with the chunks withheld\n",
            decided.out
        )
    );
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn resolve_applies_a_map_mark_learns_the_rule_and_converges() {
    let stub = model().await;
    let node = node(Some(&format!("http://{}", stub.addr)));
    let decided = sanitize_with_decide(&node, true).await;
    let report = std::fs::read_to_string(&decided.report).expect("report");
    std::fs::write(
        &decided.report,
        report.replacen("Mark: open", &format!("Mark: map {RISKY} as <customer>"), 1),
    )
    .expect("marked report");
    let resolve = [
        "memory",
        "sanitize",
        "--resolve",
        &decided.report,
        "--mapping",
        &decided.rules,
        "--learn",
    ];

    let first = run(&resolve, &node.environment).await;

    assert_eq!(first.exit, Exit::Ok, "{}", first.stderr);
    assert_eq!(
        first.stdout,
        format!(
            "resolved 1 flag in {} (0 keep, 0 redact, 1 map); every flag is reviewed\nadded 1 map rule to {}\n",
            decided.out, decided.rules
        )
    );
    let clean = bundle_at(Path::new(&decided.out));
    let contents: Vec<&str> = clean
        .sessions
        .iter()
        .flat_map(|session| &session.batch.messages)
        .map(|message| message.content.as_str())
        .collect();
    assert_eq!(
        contents,
        [
            "Added a parser test; all 42 passed.",
            "<customer> is churning next quarter."
        ]
    );
    assert!(matches!(
        &clean.manifest.sanitization,
        Sanitization::Sanitized(stamp) if stamp.unresolved_flags == 0
    ));
    assert_eq!(
        std::fs::read_to_string(&decided.rules).expect("mapping"),
        "[[rule]]\nregex = '(?u:\\b)Acme Corp(?u:\\b)'\naction = \"map\"\nas = \"<customer>\"\n"
    );

    let again = run(&resolve, &node.environment).await;

    assert_eq!(
        again.stdout,
        format!(
            "resolved 0 flags in {} (0 keep, 0 redact, 0 map); every flag is reviewed\n{} already has a rule for each map mark\n",
            decided.out, decided.rules
        )
    );
    assert_eq!(bundle_at(Path::new(&decided.out)), clean);
    let ingest = run(
        &["memory", "ingest-bundle", &decided.out, "--dry-run"],
        &node.environment,
    )
    .await;
    assert!(
        ingest
            .stdout
            .ends_with("would ingest 1 session from phil; nothing written\n"),
        "{}",
        ingest.stdout
    );
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn an_unreachable_model_fails_and_writes_nothing() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("free port");
    let node = node(Some(&format!("http://{closed}")));
    let raw = raw_bundle(node.directory.path());
    let out = node.directory.path().join("s.clean.json");

    let report = run(
        &[
            "memory",
            "sanitize",
            &raw,
            "--out",
            &out.display().to_string(),
            "--decide",
        ],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report.stderr.starts_with(&format!(
            "Error: the decision pass failed, so no bundle was written: the decision model at http://{closed}/v1/decide/info could not be reached"
        )),
        "{}",
        report.stderr
    );
    assert_eq!(listing(node.directory.path()), ["s.json", "users.json"]);
}

#[tokio::test]
async fn decide_without_a_configured_model_says_which_setting_to_add() {
    let node = node(None);
    let raw = raw_bundle(node.directory.path());

    let report = run(
        &[
            "memory",
            "sanitize",
            &raw,
            "--out",
            "unused.json",
            "--decide",
        ],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Failure);
    assert_eq!(
        report.stderr,
        "Error: --decide needs memory.sanitize.decide.url in the config (the decision model, for example http://<host>:8015)\n"
    );
}

#[tokio::test]
async fn block_needs_decide() {
    let node = node(None);
    let raw = raw_bundle(node.directory.path());

    let report = run(
        &[
            "memory",
            "sanitize",
            &raw,
            "--out",
            "unused.json",
            "--block",
        ],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Failure);
    assert_eq!(
        report.stderr,
        "Error: --block makes the decision-model review block ingest and needs --decide\n"
    );
}

#[tokio::test]
async fn plain_sanitize_with_a_mapping_never_reads_the_config() {
    let node = node(None);
    std::fs::write(
        node.home.path().join(".rivethub/config.yaml"),
        "memory: [this is not a mapping\n",
    )
    .expect("broken config");
    let raw = raw_bundle(node.directory.path());
    let rules = node.directory.path().join("rules.toml");
    std::fs::write(&rules, "").expect("empty mapping");
    let out = node.directory.path().join("s.clean.json");

    let report = run(
        &[
            "memory",
            "sanitize",
            &raw,
            "--out",
            &out.display().to_string(),
            "--mapping",
            &rules.display().to_string(),
        ],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
}

#[tokio::test]
async fn decide_concurrency_is_capped_for_the_shared_model() {
    let node = node(Some("http://127.0.0.1:9"));
    let config = node.home.path().join(".rivethub/config.yaml");
    let text = std::fs::read_to_string(&config).expect("config");
    std::fs::write(&config, format!("{text}      concurrency: 9\n")).expect("config");
    let raw = raw_bundle(node.directory.path());

    let report = run(
        &[
            "memory",
            "sanitize",
            &raw,
            "--out",
            "unused.json",
            "--decide",
        ],
        &node.environment,
    )
    .await;

    assert_eq!(report.exit, Exit::Failure);
    assert_eq!(
        report.stderr,
        "Error: memory.sanitize.decide.concurrency is 9; set it from 1 to 8, since the decision model is a shared server\n"
    );
}
