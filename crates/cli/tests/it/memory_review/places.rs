use cli::Exit;
use wire::JsonObject;
use wire::capture::BundledSession;
use wire::capture::FlagMark;
use wire::capture::ReviewMode;
use wire::capture::Sanitization;

use super::Model;
use super::Node;
use super::Sanitized;
use super::message;
use super::serve;
use super::session;

const INNER: &str = "Initech";
const OUTER: &str = "the Initech ledger moved";
const ELSEWHERE: &str = "unflagged: Initech again";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Open,
    Keep,
    Redact,
    Map,
}

const MARKS: [Mark; 4] = [Mark::Open, Mark::Keep, Mark::Redact, Mark::Map];

impl Mark {
    fn line(self, term: &str, placeholder: &str) -> Option<String> {
        match self {
            Self::Open => None,
            Self::Keep => Some("Mark: keep".to_owned()),
            Self::Redact => Some("Mark: redact".to_owned()),
            Self::Map => Some(format!("Mark: map {term} as {placeholder}")),
        }
    }
}

fn mode(block: bool) -> &'static [&'static str] {
    if block {
        &["--decide", "--block"]
    } else {
        &["--decide"]
    }
}

async fn ingest(node: &Node, sanitized: &Sanitized) -> cli::Report {
    let out = sanitized.out.display().to_string();
    node.run(&["memory", "ingest-bundle", &out, "--dry-run"])
        .await
}

fn accepts(ingest: &cli::Report) -> bool {
    ingest.exit == Exit::Ok && !ingest.stdout.contains("needs --force")
}

async fn nested(node: &Node, block: bool, outer: Mark, inner: Mark) -> Sanitized {
    let raw = node.raw(
        &format!("n-{block}-{outer:?}-{inner:?}.json"),
        &[&[INNER, OUTER, ELSEWHERE]],
    );
    let sanitized = node.sanitize(&raw, mode(block)).await;
    if let Some(line) = outer.line("ledger", "LEDGER") {
        sanitized.mark(1, &line);
    }
    if let Some(line) = inner.line(INNER, "ORG") {
        sanitized.mark(2, &line);
    }
    let resolve = node.resolve(&sanitized, &[]).await;
    assert_eq!(
        resolve.exit,
        Exit::Ok,
        "{block} {outer:?} {inner:?}: {}",
        resolve.stderr
    );
    sanitized
}

#[tokio::test]
async fn every_mark_on_a_flag_applies_inside_another_flagged_text() {
    let model = Model::exactly(&[(OUTER, 0.95), (INNER, 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let mut problems = Vec::new();
    for block in [false, true] {
        for outer in MARKS {
            for inner in MARKS {
                let case = format!("block={block} outer={outer:?} inner={inner:?}");
                let sanitized = nested(&node, block, outer, inner).await;
                let contents = sanitized.contents().concat();
                let text = sanitized.bundle_text();
                let open = outer == Mark::Open || inner == Mark::Open;
                if matches!(inner, Mark::Redact | Mark::Map)
                    && contents.iter().any(|line| line.contains(INNER))
                {
                    problems.push(format!("{case}: the inner text ships: {contents:?}"));
                }
                if outer == Mark::Redact && contents.iter().any(|line| line.contains("ledger")) {
                    problems.push(format!(
                        "{case}: the redacted outer text ships: {contents:?}"
                    ));
                }
                if block && inner == Mark::Open && text.contains(INNER) {
                    problems.push(format!("{case}: withheld inner text ships: {contents:?}"));
                }
                if block && open && !text.contains("[FLAGGED:") {
                    problems.push(format!(
                        "{case}: an open flag is not withheld: {contents:?}"
                    ));
                }
                let accepted = accepts(&ingest(&node, &sanitized).await);
                if accepted == (block && open) {
                    problems.push(format!("{case}: ingest accepted={accepted}"));
                }
                let before = (sanitized.bundle_text(), sanitized.report());
                let rerun = node.resolve(&sanitized, &[]).await;
                if rerun.exit != Exit::Ok || before != (sanitized.bundle_text(), sanitized.report())
                {
                    problems.push(format!(
                        "{case}: a rerun changed the files: {}",
                        rerun.stderr
                    ));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn overlapping_marks_resolve_byte_by_byte_with_redact_first() {
    let model = Model::exactly(&[(OUTER, 0.95), (INNER, 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let cases = [
        (
            false,
            Mark::Keep,
            Mark::Redact,
            [
                "[REDACTED:flagged]",
                "the [REDACTED:flagged] ledger moved",
                "unflagged: [REDACTED:flagged] again",
            ],
        ),
        (
            false,
            Mark::Map,
            Mark::Map,
            ["ORG", "the ORG LEDGER moved", "unflagged: ORG again"],
        ),
        (
            false,
            Mark::Redact,
            Mark::Keep,
            [INNER, "[REDACTED:flagged]", ELSEWHERE],
        ),
        (
            true,
            Mark::Keep,
            Mark::Open,
            [
                "[FLAGGED:2]",
                "the [FLAGGED:2] ledger moved",
                "unflagged: [FLAGGED:2] again",
            ],
        ),
        (
            true,
            Mark::Open,
            Mark::Keep,
            [INNER, "[FLAGGED:1]", ELSEWHERE],
        ),
        (
            true,
            Mark::Redact,
            Mark::Open,
            [
                "[FLAGGED:2]",
                "[REDACTED:flagged]",
                "unflagged: [FLAGGED:2] again",
            ],
        ),
    ];
    for (block, outer, inner, expected) in cases {
        let sanitized = nested(&node, block, outer, inner).await;
        assert_eq!(
            sanitized.contents(),
            [expected.to_vec()],
            "block={block} outer={outer:?} inner={inner:?}"
        );
    }
    stub.close().await.expect("stub shuts down");
}

const COVERED: &str = "Initech ledger";

fn object(text: &str) -> JsonObject {
    serde_json::from_str(text).expect("object")
}

fn covered() -> Vec<BundledSession> {
    let mut lookup = message("ran a lookup");
    lookup.tool_name = Some(COVERED.to_owned());
    lookup.tool_args = Some(object(
        r#"{"Initech ledger":"v","Initech LEDGER":"w","q":"plain words"}"#,
    ));
    lookup.metadata = Some(object(r#"{"Initech ledger":1,"note":"plain words"}"#));
    let mut first = session(0, vec![message(COVERED), lookup]);
    first.batch.task_id = Some(COVERED.to_owned());
    first.batch.channel = Some(COVERED.to_owned());
    first.batch.settings = Some(object(r#"{"Initech ledger":"s"}"#));
    vec![first]
}

fn arguments(sanitized: &Sanitized) -> Vec<String> {
    let bundle = sanitized.bundle();
    let mut values: Vec<String> = bundle
        .sessions
        .first()
        .and_then(|session| session.batch.messages.get(1))
        .and_then(|message| message.tool_args.as_ref())
        .expect("tool args")
        .as_map()
        .values()
        .map(ToString::to_string)
        .collect();
    values.sort();
    values
}

fn task_id_is_a_uuid(sanitized: &Sanitized) -> bool {
    sanitized
        .bundle()
        .sessions
        .first()
        .and_then(|session| session.batch.task_id.as_deref())
        .is_some_and(|task| uuid::Uuid::parse_str(task).is_ok())
}

#[tokio::test]
async fn flagged_text_in_keys_tool_names_task_ids_and_channels_is_covered() {
    let model = Model::exactly(&[(COVERED, 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let mut problems = Vec::new();
    for (block, mark, gone) in [
        (true, Mark::Open, COVERED),
        (true, Mark::Redact, COVERED),
        (false, Mark::Redact, COVERED),
        (true, Mark::Map, "ledger"),
        (false, Mark::Map, "ledger"),
    ] {
        let case = format!("block={block} mark={mark:?}");
        let raw = node.raw_sessions(&format!("cover-{block}-{mark:?}.json"), covered());
        let sanitized = node.sanitize(&raw, mode(block)).await;
        if let Some(line) = mark.line("ledger", "LEDGER") {
            sanitized.mark(1, &line);
            let resolve = node.resolve(&sanitized, &[]).await;
            assert_eq!(resolve.exit, Exit::Ok, "{case}: {}", resolve.stderr);
        }
        let text = sanitized.bundle_text();
        if text.contains(gone) {
            problems.push(format!("{case}: `{gone}` ships in {text}"));
        }
        if !task_id_is_a_uuid(&sanitized) {
            problems.push(format!("{case}: the task id is not a UUID"));
        }
        if arguments(&sanitized) != ["\"plain words\"", "\"v\"", "\"w\""] {
            problems.push(format!(
                "{case}: tool args lost a value: {:?}",
                arguments(&sanitized)
            ));
        }
        if mark != Mark::Open && !accepts(&ingest(&node, &sanitized).await) {
            problems.push(format!("{case}: ingest refused the reviewed bundle"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    stub.close().await.expect("stub shuts down");
}

fn after_first(sanitized: &Sanitized) -> Vec<String> {
    sanitized
        .contents()
        .concat()
        .get(1..)
        .unwrap_or_default()
        .to_vec()
}

const ASSIGNMENT: &str = concat!("api_key: ", "PLANTEDassignCO07");

#[tokio::test]
async fn a_flag_never_reaches_into_a_placeholder() {
    let mut problems = Vec::new();
    for flagged in ["ED", "REDACTED", "secret_assignment", "FLAGGED"] {
        let model = Model::exactly(&[(flagged, 0.9)]);
        let stub = serve(&model).await;
        let node = Node::new(&format!("http://{}", stub.addr), 5);
        let lines = [flagged, ASSIGNMENT, "see [FLAGGED:4] here"];
        let plain = node.sanitize(&node.raw("plain.json", &[&lines]), &[]).await;
        let expected = after_first(&plain);
        for (block, mark) in [
            (true, Mark::Open),
            (true, Mark::Keep),
            (true, Mark::Redact),
            (false, Mark::Redact),
            (false, Mark::Map),
        ] {
            let case = format!("{flagged} block={block} mark={mark:?}");
            let raw = node.raw(&format!("p-{block}-{mark:?}.json"), &[&lines]);
            let out = raw.replace(".json", ".clean.json");
            let rules = node.path("rules.toml");
            let mut args = vec![
                "memory",
                "sanitize",
                &raw,
                "--out",
                &out,
                "--mapping",
                &rules,
            ];
            args.extend_from_slice(mode(block));
            let run = node.run(&args).await;
            if run.exit != Exit::Ok {
                problems.push(format!("{case}: sanitize refused: {}", run.stderr));
                continue;
            }
            let sanitized = Sanitized {
                report: out.replace(".clean.json", ".clean.report.md").into(),
                out: out.clone().into(),
                stdout: run.stdout,
            };
            if let Some(line) = mark.line(flagged, "Zork") {
                sanitized.mark(1, &line);
                let resolve = node.resolve(&sanitized, &[]).await;
                if resolve.exit != Exit::Ok {
                    problems.push(format!("{case}: resolve refused: {}", resolve.stderr));
                    continue;
                }
                if !accepts(&ingest(&node, &sanitized).await) {
                    problems.push(format!("{case}: ingest refused"));
                }
            }
            let contents = after_first(&sanitized);
            if contents != expected {
                problems.push(format!(
                    "{case}: placeholders changed: {contents:?} vs {expected:?}"
                ));
            }
        }
        stub.close().await.expect("stub shuts down");
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ShippedReview {
    mode: ReviewMode,
    model: String,
    calls: usize,
    flags: Vec<ShippedFlag>,
    seal: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ShippedFlag {
    number: usize,
    mark: FlagMark,
}

fn shipped(sanitized: &Sanitized) -> Option<ShippedReview> {
    let Sanitization::Sanitized(stamp) = sanitized.bundle().manifest.sanitization else {
        return None;
    };
    let text = serde_json::to_string(&stamp.review?).expect("review serializes");
    serde_json::from_str(&text).ok()
}

const CHURN: &str = "Acme Corp is churning next quarter.";
const LAYOFFS: &str = "Initech layoffs are planned for March.";

#[tokio::test]
async fn the_bundle_never_shows_where_the_flagged_text_is() {
    let flagging = Model::scoring(&[("Acme Corp", 0.9), ("Initech", 0.8)]);
    let flagging_stub = serve(&flagging).await;
    let quiet = Model::scoring(&[]);
    let quiet_stub = serve(&quiet).await;
    let node = Node::new(&format!("http://{}", flagging_stub.addr), 5);
    let lines = ["Added a parser test; all 42 passed.", CHURN, LAYOFFS];
    let flagged = node
        .sanitize(&node.raw("a.json", &[&lines]), &["--decide"])
        .await;
    let other = Node::new(&format!("http://{}", quiet_stub.addr), 5);
    std::fs::copy(flagged_key(&node), flagged_key(&other)).expect("same key on both runs");
    let unflagged = other
        .sanitize(&other.raw("a.json", &[&lines]), &["--decide"])
        .await;

    assert_eq!(flagged.bundle().sessions, unflagged.bundle().sessions);
    let review = shipped(&flagged).expect("the stamp holds only counts, numbers, marks and a seal");
    assert_eq!(review.mode, ReviewMode::Advisory);
    assert_eq!(review.model, "stub-model");
    assert!(review.calls > 0 && !review.seal.is_empty());
    assert_eq!(
        review
            .flags
            .iter()
            .map(|flag| (flag.number, flag.mark))
            .collect::<Vec<_>>(),
        [(1, FlagMark::Open), (2, FlagMark::Open)]
    );
    let local = flagged.out.with_file_name("a.clean.review.local.json");
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&local)
            .expect("local review file")
            .permissions(),
    );
    assert_eq!(mode & 0o777, 0o600);
    flagging_stub.close().await.expect("stub shuts down");
    quiet_stub.close().await.expect("stub shuts down");
}

fn flagged_key(node: &Node) -> std::path::PathBuf {
    let home = node
        .environment
        .text("HOME")
        .map(std::path::PathBuf::from)
        .expect("home");
    let key = capture::sanitize::LocalKey::path_in(&home.join(".local").join("state"));
    std::fs::create_dir_all(key.parent().expect("key directory")).expect("key directory");
    key
}

#[tokio::test]
async fn ingest_refuses_an_unreviewed_count_that_disagrees_with_the_review() {
    let model = Model::scoring(&[("Acme Corp", 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let sanitized = node
        .sanitize(&node.raw("s.json", &[&[CHURN]]), &["--decide"])
        .await;
    let bundle = sanitized.bundle();
    let Sanitization::Sanitized(mut stamp) = bundle.manifest.sanitization.clone() else {
        panic!("sanitized");
    };
    stamp.unresolved_flags = 0;
    let forged = capture::bundle::seal(
        bundle.manifest.exported_by.clone(),
        bundle.manifest.exported_at,
        Sanitization::Sanitized(stamp),
        bundle.sessions.clone(),
    )
    .expect("reseal");
    std::fs::write(&sanitized.out, forged.text).expect("forged bundle");

    let ingest = ingest(&node, &sanitized).await;

    assert_eq!(ingest.exit, Exit::Failure, "{}", ingest.stdout);
    assert!(
        ingest.stderr.contains("unresolved_flags"),
        "{}",
        ingest.stderr
    );
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn a_review_from_another_machine_is_refused_by_name() {
    let model = Model::scoring(&[("Acme Corp", 0.9)]);
    let stub = serve(&model).await;
    let url = format!("http://{}", stub.addr);
    let node = Node::new(&url, 5);
    let sanitized = node
        .sanitize(&node.raw("s.json", &[&[CHURN]]), &["--decide", "--block"])
        .await;
    sanitized.mark(1, "Mark: keep");
    let other = Node::new(&url, 5);
    for name in [
        "s.clean.json",
        "s.clean.report.md",
        "s.clean.review.local.json",
    ] {
        let from = node.directory.path().join(name);
        if from.exists() {
            std::fs::copy(&from, other.directory.path().join(name)).expect("copy");
        }
    }
    let copied = Sanitized {
        out: other.directory.path().join("s.clean.json"),
        report: other.directory.path().join("s.clean.report.md"),
        stdout: String::new(),
    };
    let before = copied.bundle_text();

    let resolve = other.resolve(&copied, &[]).await;

    assert_eq!(resolve.exit, Exit::Failure, "{}", resolve.stdout);
    assert!(
        resolve.stderr.contains("another machine"),
        "{}",
        resolve.stderr
    );
    assert_eq!(copied.bundle_text(), before);
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn an_edit_anywhere_in_the_bundle_is_refused() {
    let model = Model::scoring(&[("Acme Corp", 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let sanitized = node
        .sanitize(
            &node.raw("s.json", &[&[CHURN, "plain words"]]),
            &["--decide", "--block"],
        )
        .await;
    let mut bundle = sanitized.bundle();
    let Some(message) = bundle
        .sessions
        .first_mut()
        .and_then(|session| session.batch.messages.get_mut(1))
    else {
        panic!("two messages");
    };
    message.content = CHURN.to_owned();
    let edited = capture::bundle::seal(
        bundle.manifest.exported_by.clone(),
        bundle.manifest.exported_at,
        bundle.manifest.sanitization.clone(),
        bundle.sessions,
    )
    .expect("reseal");
    std::fs::write(&sanitized.out, edited.text).expect("edited bundle");
    sanitized.mark(1, "Mark: redact");
    let before = sanitized.bundle_text();

    let resolve = node.resolve(&sanitized, &[]).await;

    assert_eq!(resolve.exit, Exit::Failure, "{}", resolve.stdout);
    assert!(resolve.stderr.contains("changed"), "{}", resolve.stderr);
    assert_eq!(sanitized.bundle_text(), before);
    stub.close().await.expect("stub shuts down");
}
