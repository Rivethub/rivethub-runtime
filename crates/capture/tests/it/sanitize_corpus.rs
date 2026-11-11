use std::path::Path;
use std::path::PathBuf;

use capture::sanitize;
use capture::sanitize::LocalKey;
use capture::sanitize::Policy;
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
pub(super) enum Position {
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
pub(super) enum Expect {
    Redact,
    Keep,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Case {
    pub(super) shape: String,
    pub(super) position: String,
    pub(super) raw: String,
    #[serde(default)]
    pub(super) planted: Vec<String>,
    pub(super) expect: Expect,
    #[serde(default)]
    note: Option<String>,
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES)
}

pub(super) fn cases(corpus: &str) -> Result<Vec<Case>, Box<dyn std::error::Error>> {
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

pub(super) fn positions(spec: &str) -> Result<Vec<Position>, serde_json::Error> {
    if spec == EVERY_POSITION {
        return Ok(ALL.to_vec());
    }
    serde_json::from_str(&serde_json::to_string(spec)?).map(|position| vec![position])
}

pub(super) fn at() -> Result<DateTime<Utc>, chrono::ParseError> {
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

pub(super) fn bundle_with(
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

pub(super) fn corpus_dirs() -> Result<Vec<String>, std::io::Error> {
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
