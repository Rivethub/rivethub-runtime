use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;

use capture::sanitize;
use capture::sanitize::LocalKey;
use capture::sanitize::MarkState;
use capture::sanitize::Policy;
use capture::sanitize::ReportFlags;
use capture::sanitize::Sanitized;
use capture::sanitize::decide::Endpoint;
use capture::sanitize::decide::Mode;
use capture::sanitize::decide::Resolution;
use capture::sanitize::decide::Review;
use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use wire::JsonObject;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;
use wire::capture::SessionOrigin;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const FIXTURES: &str = "tests/fixtures/sanitize";
const EVERY_POSITION: &str = "*";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Position {
    Content,
    ToolResult,
    ToolArgsValue,
    ToolArgsKey,
    ToolArgsJson,
    MetadataValue,
    MetadataKey,
    Title,
    SettingsValue,
    SessionKey,
    Channel,
    TaskId,
    ToolName,
    Agent,
    OriginNode,
    EventId,
}

const ALL: [Position; 15] = [
    Position::Content,
    Position::ToolResult,
    Position::ToolArgsValue,
    Position::ToolArgsKey,
    Position::MetadataValue,
    Position::MetadataKey,
    Position::Title,
    Position::SettingsValue,
    Position::SessionKey,
    Position::Channel,
    Position::TaskId,
    Position::ToolName,
    Position::Agent,
    Position::OriginNode,
    Position::EventId,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Expect {
    Redact,
    Keep,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    shape: String,
    position: String,
    raw: String,
    #[serde(default)]
    planted: Vec<String>,
    expect: Expect,
    #[serde(default)]
    note: Option<String>,
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES)
}

fn cases(corpus: &str) -> Result<Vec<Case>, Box<dyn std::error::Error>> {
    let path = fixture_dir().join(corpus).join("cases.jsonl");
    let text =
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let case: Case = serde_json::from_str(line)
            .map_err(|error| format!("{} line {}: {error}", path.display(), index + 1))?;
        out.push(case);
    }
    Ok(out)
}

fn positions(spec: &str) -> Result<Vec<Position>, serde_json::Error> {
    if spec == EVERY_POSITION {
        return Ok(ALL.to_vec());
    }
    serde_json::from_str(&serde_json::to_string(spec)?).map(|position| vec![position])
}

fn at() -> Result<DateTime<Utc>, chrono::ParseError> {
    Ok(DateTime::parse_from_rfc3339("2026-11-08T12:00:00Z")?.with_timezone(&Utc))
}

fn object(text: &str) -> Result<JsonObject, serde_json::Error> {
    serde_json::from_str(text)
}

fn quoted(text: &str) -> Result<String, serde_json::Error> {
    serde_json::to_string(text)
}

fn blank_message() -> CaptureMessage {
    CaptureMessage {
        event_id: "evt-1".to_owned(),
        role: Role::Assistant,
        content: "hello".to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    }
}

fn blank_batch() -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: String::new(),
        agent: String::new(),
        channel: Some("claude-code".to_owned()),
        title: Some("a session".to_owned()),
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: Vec::new(),
    }
}

fn bundle_with(
    position: Position,
    text: &str,
) -> Result<SessionBundle, Box<dyn std::error::Error>> {
    let mut node = "node-1".to_owned();
    let mut session_key = "claude-code:sess-1".to_owned();
    let mut agent = "claude".to_owned();
    let mut message = blank_message();
    let mut batch = blank_batch();
    match position {
        Position::Content => text.clone_into(&mut message.content),
        Position::ToolResult => message.tool_result = Some(text.to_owned()),
        Position::ToolArgsValue => {
            let q = quoted(text)?;
            message.tool_args = Some(object(&format!(
                r#"{{"cmd": {{"argv": ["sh", "-c", {q}], "opts": {{"note": {q}}}}}, "nest": [{{"deep": {{"text": {q}}}}}]}}"#
            ))?);
        }
        Position::ToolArgsKey => {
            message.tool_args = Some(object(&format!(r#"{{{}: "x"}}"#, quoted(text)?))?);
        }
        Position::ToolArgsJson => message.tool_args = Some(object(text)?),
        Position::MetadataValue => {
            message.metadata = Some(object(&format!(
                r#"{{"ctx": {{"note": {}}}}}"#,
                quoted(text)?
            ))?);
        }
        Position::MetadataKey => {
            message.metadata = Some(object(&format!("{{{}: 1}}", quoted(text)?))?);
        }
        Position::Title => batch.title = Some(text.to_owned()),
        Position::SettingsValue => {
            batch.settings = Some(object(&format!(
                r#"{{"profile": {{"note": {}}}}}"#,
                quoted(text)?
            ))?);
        }
        Position::SessionKey => text.clone_into(&mut session_key),
        Position::Channel => batch.channel = Some(text.to_owned()),
        Position::TaskId => batch.task_id = Some(text.to_owned()),
        Position::ToolName => message.tool_name = Some(text.to_owned()),
        Position::Agent => text.clone_into(&mut agent),
        Position::OriginNode => text.clone_into(&mut node),
        Position::EventId => text.clone_into(&mut message.event_id),
    }
    batch.session_key.clone_from(&session_key);
    batch.agent.clone_from(&agent);
    batch.messages = vec![message];
    let session = BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: node.clone(),
            conversation_id: "conv-1".to_owned(),
            session_key,
            agent,
            created_at: None,
            updated_at: None,
            identity: None,
        },
        batch,
    };
    Ok(capture::bundle::seal(
        BundleAuthor {
            user: "phil".to_owned(),
            node,
        },
        at()?,
        Sanitization::Raw,
        vec![session],
    )?
    .bundle)
}

fn field_after(bundle: &SessionBundle, position: Position) -> Option<String> {
    let session = bundle.sessions.first()?;
    let message = session.batch.messages.first()?;
    match position {
        Position::Content => Some(message.content.clone()),
        Position::ToolResult => message.tool_result.clone(),
        Position::Title => session.batch.title.clone(),
        Position::ToolArgsJson => message
            .tool_args
            .as_ref()
            .and_then(|args| serde_json::to_string(args).ok()),
        Position::ToolArgsValue
        | Position::ToolArgsKey
        | Position::MetadataValue
        | Position::MetadataKey
        | Position::SettingsValue
        | Position::SessionKey
        | Position::Channel
        | Position::TaskId
        | Position::ToolName
        | Position::Agent
        | Position::OriginNode
        | Position::EventId => None,
    }
}

fn check(case: &Case, position: Position) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let input = bundle_with(position, &case.raw)?;
    let before = serde_json::to_string(&input)?;
    let mut found = Vec::new();
    let tag =
        |where_: &str, what: &str| format!("{} | {position:?} | {where_} | {what}", case.shape);
    for value in &case.planted {
        if !before.contains(value) {
            found.push(tag("input", &format!("does not plant {value}")));
        }
    }
    let sanitized = match sanitize::run(
        &input,
        &Policy {
            mapping: sanitize::mapping::Mapping::default(),
            key: LocalKey::from_bytes([7; 32]),
        },
        at()?,
    ) {
        Ok(sanitized) => sanitized,
        Err(error) => return Ok(vec![tag("run", &format!("refused: {error}"))]),
    };
    let file = String::from_utf8(sanitized.sealed.text.clone())?;
    match case.expect {
        Expect::Redact => {
            let report = sanitized.report.markdown("s.clean.json", "no rules");
            for value in &case.planted {
                let escaped = serde_json::to_string(value)?;
                let escaped = escaped.trim_matches('"');
                if file.contains(value) || file.contains(escaped) {
                    found.push(tag("sealed text", value));
                }
                if report.contains(value) {
                    found.push(tag("report", value));
                }
            }
        }
        Expect::Keep => {
            let expected = if position == Position::ToolArgsJson {
                serde_json::to_string(&object(&case.raw)?)?
            } else {
                case.raw.clone()
            };
            match field_after(&sanitized.sealed.bundle, position) {
                Some(after) if after == expected => {}
                Some(after) => found.push(tag("changed", &after.replace('\n', "\\n"))),
                None => found.push(tag("keep", "position cannot be read back")),
            }
        }
    }
    Ok(found)
}

fn rescan(case: &Case, position: Position) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let sanitized = sanitize::run(
        &bundle_with(position, &case.raw)?,
        &Policy {
            mapping: sanitize::mapping::Mapping::default(),
            key: LocalKey::from_bytes([7; 32]),
        },
        at()?,
    )?;
    let mut found: Vec<String> = sanitize::secrets(&sanitized.sealed.bundle)
        .iter()
        .map(|again| format!("{} | {position:?} | {again}", case.shape))
        .collect();
    if sanitize::withheld(&sanitized.sealed.bundle) > 0 {
        found.push(format!(
            "{} | {position:?} | ingest would count a withheld flag",
            case.shape
        ));
    }
    if sanitized.report.rechecked > 0 {
        found.push(format!(
            "{} | {position:?} | {} span(s) caught only by the re-check",
            case.shape, sanitized.report.rechecked
        ));
    }
    Ok(found)
}

#[test]
fn every_corpus_case_sanitizes_to_a_bundle_ingest_accepts() -> TestResult {
    let mut found = Vec::new();
    let mut runs = 0;
    for corpus in corpus_dirs()? {
        for case in &cases(&corpus)? {
            for position in positions(&case.position)? {
                runs += 1;
                found.extend(
                    rescan(case, position)?
                        .into_iter()
                        .map(|line| format!("{corpus} | {line}")),
                );
            }
        }
    }
    assert!(runs > 0, "no corpus cases ran");
    assert!(
        found.is_empty(),
        "{} of {runs} sanitized bundles hold a match ingest refuses or needed the re-check (a sanitizer bug either way)\ncorpus | shape | position | match\n{}",
        found.len(),
        found.join("\n")
    );
    Ok(())
}

#[derive(serde::Serialize)]
struct Answer {
    probabilities: [f64; 2],
}

async fn flag_everything() -> axum::Json<Answer> {
    axum::Json(Answer {
        probabilities: [0.1, 0.9],
    })
}

type SendError = Box<dyn std::error::Error + Send + Sync>;

async fn decided(url: &str, raw: &SessionBundle, mode: Mode) -> Result<Sanitized, SendError> {
    let policy = Policy {
        mapping: sanitize::mapping::Mapping::default(),
        key: LocalKey::from_bytes([7; 32]),
    };
    let mut scrubbed = sanitize::scrub(raw, &policy)?;
    let candidates = scrubbed.candidates();
    let endpoint = Endpoint {
        url: url.to_owned(),
        concurrency: NonZeroUsize::MIN.saturating_add(7),
    };
    let scores = sanitize::decide::score(&endpoint, &candidates.chunks).await?;
    let review = Review {
        top: NonZeroUsize::MIN.saturating_add(19),
        threshold: 0.3,
        mode,
    };
    scrubbed.flag(candidates, &scores, review);
    Ok(scrubbed.seal(&policy, at()?)?)
}

fn marked(report: &str) -> Result<ReportFlags, SendError> {
    let mut flags = sanitize::marks::read_flags(report)?;
    for flag in &mut flags.flags {
        let term = flag.original.split_whitespace().next();
        flag.state = MarkState::Marked(match (flag.number % 3, term) {
            (0, _) => Resolution::Keep,
            (2, Some(term)) => Resolution::Map {
                term: term.to_owned(),
                placeholder: "[ORG]".to_owned(),
            },
            _ => Resolution::Redact,
        });
    }
    Ok(flags)
}

async fn decide_stages(
    url: &str,
    case: &Case,
    position: Position,
) -> Result<Vec<String>, SendError> {
    let raw = bundle_with(position, &case.raw).map_err(|error| error.to_string())?;
    let mut found = Vec::new();
    for mode in [Mode::Advisory, Mode::Block] {
        let line = |stage: &str, what: &dyn std::fmt::Display| {
            format!("{} | {position:?} | {mode} {stage} | {what}", case.shape)
        };
        let sanitized = match decided(url, &raw, mode).await {
            Ok(sanitized) => sanitized,
            Err(error) => {
                found.push(line("sanitize", &error));
                continue;
            }
        };
        let report = sanitized.report.markdown("s.clean.json", "none");
        let resolved = match sanitize::resolve(&sanitized.sealed.bundle, &marked(&report)?) {
            Ok(resolved) => resolved,
            Err(error) => {
                found.push(line("resolve", &error));
                continue;
            }
        };
        for (stage, bundle) in [
            ("sanitized", &sanitized.sealed.bundle),
            ("resolved", &resolved.sealed.bundle),
        ] {
            found.extend(
                sanitize::secrets(bundle)
                    .iter()
                    .map(|again| line(stage, again)),
            );
        }
        if mode == Mode::Advisory && sanitize::withheld(&sanitized.sealed.bundle) > 0 {
            found.push(line("sanitized", &"ingest would count a withheld flag"));
        }
        if resolved.open > 0 || sanitize::withheld(&resolved.sealed.bundle) > 0 {
            found.push(line(
                "resolved",
                &"flags still open after every flag was marked",
            ));
        }
    }
    Ok(found)
}

#[tokio::test(flavor = "multi_thread")]
async fn every_corpus_case_with_the_decision_review_stays_a_bundle_ingest_accepts() -> TestResult {
    let routes = axum::Router::new().route("/v1/decide", axum::routing::post(flag_everything));
    let stub = testkit::stub_server(routes).await?;
    let url = format!("http://{}", stub.addr);
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(
        std::thread::available_parallelism().map_or(4, NonZeroUsize::get),
    ));
    let mut tasks = tokio::task::JoinSet::new();
    for corpus in corpus_dirs()? {
        for case in cases(&corpus)? {
            let case = std::sync::Arc::new(case);
            for position in positions(&case.position)? {
                let (corpus, case, url) = (corpus.clone(), case.clone(), url.clone());
                let slots = slots.clone();
                tasks.spawn(async move {
                    let _slot = slots.acquire_owned().await?;
                    let lines = decide_stages(&url, &case, position).await?;
                    Ok::<_, SendError>(
                        lines
                            .into_iter()
                            .map(|line| format!("{corpus} | {line}"))
                            .collect::<Vec<_>>(),
                    )
                });
            }
        }
    }
    let runs = tasks.len();
    let mut found = Vec::new();
    while let Some(lines) = tasks.join_next().await {
        found.extend(lines?.map_err(|error| error.to_string())?);
    }
    found.sort();
    stub.close().await?;
    assert!(runs > 0, "no corpus cases ran");
    assert!(
        found.is_empty(),
        "{} problems over {runs} corpus runs with --decide, advisory and --block, before and after --resolve\ncorpus | shape | position | stage | match\n{}",
        found.len(),
        found.join("\n")
    );
    Ok(())
}

fn corpus_dirs() -> Result<Vec<String>, std::io::Error> {
    let mut dirs: Vec<String> = std::fs::read_dir(fixture_dir())?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    Ok(dirs)
}

fn corpus_holds(corpus: &str) -> TestResult {
    let cases = cases(corpus)?;
    assert!(!cases.is_empty(), "{corpus} has no cases");
    let mut found = Vec::new();
    let mut runs = 0;
    for case in &cases {
        for position in positions(&case.position)? {
            runs += 1;
            let mut failures = check(case, position)?;
            if let (Some(note), false) = (&case.note, failures.is_empty()) {
                for failure in &mut failures {
                    failure.push_str(" (");
                    failure.push_str(note);
                    failure.push(')');
                }
            }
            found.extend(failures);
        }
    }
    assert!(
        found.is_empty(),
        "{corpus}: {} failures over {runs} runs of {} cases (see {FIXTURES}/README.md)\nshape | position | where | value\n{}",
        found.len(),
        cases.len(),
        found.join("\n")
    );
    Ok(())
}

macro_rules! corpus_tests {
    ($($test:ident => $corpus:literal;)+) => {
        $(
            #[test]
            fn $test() -> TestResult {
                corpus_holds($corpus)
            }
        )+
    };
}

corpus_tests! {
    vendor_shapes_from_round_1_leave_no_survivor => "vendor-shapes-r1";
    unseen_shapes_from_round_2_leave_no_survivor => "unseen-shapes-r2";
    vendor_and_code_shapes_from_round_3_leave_no_survivor => "vendor-and-code-shapes-r3";
    placement_and_assignment_forms_leave_no_survivor => "placement-and-forms";
    real_output_contexts_keep_their_text_and_lose_planted_secrets => "real-output-contexts-r3";
    round_3_fixes_hold => "round-3-fixes";
    round_4_fixes_hold => "round-4-fixes";
}

#[test]
fn every_corpus_directory_has_a_test() -> TestResult {
    let dirs = corpus_dirs()?;
    let mut listed = vec![
        "vendor-shapes-r1",
        "unseen-shapes-r2",
        "vendor-and-code-shapes-r3",
        "placement-and-forms",
        "real-output-contexts-r3",
        "round-3-fixes",
        "round-4-fixes",
    ];
    listed.sort_unstable();
    assert_eq!(
        dirs, listed,
        "add a corpus_tests! line for each new corpus directory"
    );
    Ok(())
}
