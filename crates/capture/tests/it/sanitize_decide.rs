use std::num::NonZeroUsize;
use std::sync::Arc;

use capture::sanitize;
use capture::sanitize::LocalKey;
use capture::sanitize::Policy;
use capture::sanitize::Sanitized;
use capture::sanitize::SessionRules;
use capture::sanitize::decide::DecideError;
use capture::sanitize::decide::Endpoint;
use capture::sanitize::decide::Mode;
use capture::sanitize::decide::Review;
use capture::sanitize::marks::MarkProblem;
use capture::sanitize::marks::ReportError;
use chrono::DateTime;
use chrono::Utc;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;
use wire::capture::SessionOrigin;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const RISKY: &str = "Acme Corp";
const WARM: &str = "churn";
const UUID: &str = "0f3c2a9e-6d1b-4c55-9a0e-2b7d8f1c4e60";

#[derive(serde::Deserialize)]
struct Ask {
    state: String,
}

#[derive(serde::Serialize)]
struct Answer {
    probabilities: [f64; 2],
}

#[derive(Clone, Default)]
struct Calls(Arc<parking_lot::Mutex<Vec<String>>>);

impl Calls {
    fn states(&self) -> Vec<String> {
        let mut states = self.0.lock().clone();
        states.sort();
        states
    }
}

async fn decide(
    axum::extract::State(calls): axum::extract::State<Calls>,
    axum::Json(ask): axum::Json<Ask>,
) -> axum::Json<Answer> {
    let state = ask.state;
    calls.0.lock().push(state.clone());
    let yes = if state.contains(RISKY) {
        0.91
    } else if state.contains(WARM) {
        0.55
    } else {
        0.06
    };
    axum::Json(Answer {
        probabilities: [1.0 - yes, yes],
    })
}

async fn info() -> &'static str {
    r#"{"model": "stub-model"}"#
}

async fn model() -> Result<(testkit::Stub, Calls), std::io::Error> {
    let calls = Calls::default();
    let routes = axum::Router::new()
        .route("/v1/decide", axum::routing::post(decide))
        .route("/v1/decide/info", axum::routing::get(info))
        .with_state(calls.clone());
    Ok((testkit::stub_server(routes).await?, calls))
}

fn endpoint(url: String) -> Endpoint {
    Endpoint {
        url,
        concurrency: NonZeroUsize::MIN,
    }
}

fn review(top: usize, mode: Mode) -> Result<Review, &'static str> {
    Ok(Review {
        top: NonZeroUsize::new(top).ok_or("top is 0")?,
        threshold: 0.3,
        mode,
    })
}

fn message(event_id: &str, content: &str) -> CaptureMessage {
    CaptureMessage {
        event_id: event_id.to_owned(),
        role: Role::Assistant,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    }
}

fn bundle() -> Result<SessionBundle, Box<dyn std::error::Error>> {
    let mut tool = message("evt-2", "running it");
    tool.tool_args = Some(serde_json::from_str(&format!(
        r#"{{"command": "grep {WARM} {RISKY}.csv", "file_path": "/srv/reports/q3.csv", "limit": 40}}"#
    ))?);
    tool.tool_result = Some(format!("{WARM} rows: 3"));
    tool.metadata = Some(serde_json::from_str(&format!(
        r#"{{"uuid": "{UUID}", "sidechain": false}}"#
    ))?);
    let mut repeat = message("evt-4", "running it");
    repeat.tool_args.clone_from(&tool.tool_args);
    let session = BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: "desk".to_owned(),
            conversation_id: "c1".to_owned(),
            session_key: "claude-code:c1".to_owned(),
            agent: "rivet".to_owned(),
            created_at: None,
            updated_at: None,
            identity: None,
        },
        batch: CaptureBatchRequest {
            session_key: "claude-code:c1".to_owned(),
            agent: "rivet".to_owned(),
            channel: None,
            title: Some("numbers for Q3".to_owned()),
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![
                message(
                    "evt-1",
                    &format!("Ran the tests.\n{RISKY} is churning next quarter."),
                ),
                tool,
                message("evt-3", "running it"),
                repeat,
            ],
        },
    };
    let raw = capture::bundle::seal(
        BundleAuthor {
            user: "phil".to_owned(),
            node: "desk".to_owned(),
        },
        DateTime::UNIX_EPOCH,
        Sanitization::Raw,
        vec![session],
    )?;
    Ok(raw.bundle)
}

fn at() -> Result<DateTime<Utc>, chrono::ParseError> {
    Ok(DateTime::parse_from_rfc3339("2026-11-08T12:00:00.123456789Z")?.with_timezone(&Utc))
}

fn policy() -> Result<Policy, sanitize::mapping::MappingError> {
    Ok(Policy {
        mapping: sanitize::mapping::parse("")?,
        key: LocalKey::from_bytes([7; 32]),
        sessions: SessionRules::default(),
    })
}

async fn decided(url: String, review: Review) -> Result<Sanitized, Box<dyn std::error::Error>> {
    let policy = policy()?;
    let mut scrubbed = sanitize::scrub(&bundle()?, &policy)?;
    let candidates = scrubbed.candidates();
    let scores = sanitize::decide::score(&endpoint(url), &candidates.chunks).await?;
    scrubbed.flag(candidates, &scores, review)?;
    Ok(scrubbed.seal(&policy, at()?)?)
}

async fn advisory(url: String) -> Result<Sanitized, Box<dyn std::error::Error>> {
    decided(url, review(20, Mode::Advisory)?).await
}

async fn blocking(url: String) -> Result<Sanitized, Box<dyn std::error::Error>> {
    decided(url, review(20, Mode::Block)?).await
}

fn contents(bundle: &SessionBundle) -> Vec<&str> {
    bundle
        .sessions
        .iter()
        .flat_map(|session| &session.batch.messages)
        .map(|message| message.content.as_str())
        .collect()
}

#[derive(serde::Deserialize)]
struct Command {
    command: String,
}

fn command(bundle: &SessionBundle, index: usize) -> Option<String> {
    let arguments = bundle
        .sessions
        .first()?
        .batch
        .messages
        .get(index)?
        .tool_args
        .as_ref()?;
    let typed: Command = serde_json::from_str(&serde_json::to_string(arguments).ok()?).ok()?;
    Some(typed.command)
}

fn stamp_flags(bundle: &SessionBundle) -> Option<u32> {
    match &bundle.manifest.sanitization {
        Sanitization::Sanitized(stamp) => Some(stamp.unresolved_flags),
        Sanitization::Raw => None,
    }
}

fn risky_line() -> String {
    format!("Ran the tests.\n{RISKY} is churning next quarter.")
}

fn event_ids(bundle: &SessionBundle) -> Vec<String> {
    bundle
        .sessions
        .iter()
        .flat_map(|session| &session.batch.messages)
        .map(|message| message.event_id.clone())
        .collect()
}

fn place(ids: &[String], message: usize, field: &str) -> String {
    let id = ids.get(message).map_or("-", String::as_str);
    format!("{id}:{field}")
}

fn risky_command() -> String {
    format!("grep {WARM} {RISKY}.csv")
}

#[tokio::test]
async fn the_top_chunks_of_each_session_are_listed_and_stay_in_the_bundle() -> TestResult {
    let (stub, calls) = model().await?;
    let sanitized = advisory(format!("http://{}", stub.addr)).await?;
    let decision = sanitized.report.decision.as_ref().ok_or("no decision")?;
    let ids = event_ids(&sanitized.sealed.bundle);
    let at = |message: usize, field: &str| place(&ids, message, field);
    let listed: Vec<(&str, Vec<String>, String)> = decision
        .flags
        .iter()
        .map(|flag| {
            (
                flag.text.as_str(),
                flag.shown
                    .iter()
                    .map(|shown| format!("{}:{}", shown.owner, shown.field))
                    .collect(),
                format!("{:.2}", flag.score),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            (
                risky_line().as_str(),
                vec![at(0, "content")],
                "0.91".to_owned()
            ),
            (
                risky_command().as_str(),
                vec![at(1, "tool_args.command"), at(3, "tool_args.command")],
                "0.91".to_owned()
            ),
            (
                format!("{WARM} rows: 3").as_str(),
                vec![at(1, "tool_result")],
                "0.55".to_owned()
            ),
        ]
    );
    assert_eq!(
        calls.states(),
        [
            "/srv/reports/q3.csv".to_owned(),
            risky_line(),
            format!("{WARM} rows: 3"),
            risky_command(),
            "numbers for Q3".to_owned(),
            "running it".to_owned(),
        ],
        "a path is sent, a UUID is not, and equal texts share one call"
    );
    assert_eq!(
        (decision.scored, decision.calls, decision.trivial),
        (10, 6, 1)
    );

    let bundle = &sanitized.sealed.bundle;
    assert_eq!(
        contents(bundle),
        [
            risky_line().as_str(),
            "running it",
            "running it",
            "running it"
        ],
        "an advisory flag keeps its sanitized text"
    );
    assert_eq!(command(bundle, 1), Some(risky_command()));
    assert_eq!(stamp_flags(bundle), Some(3));
    assert_eq!(sanitize::withheld(bundle), 0);
    stub.close().await?;
    Ok(())
}

#[tokio::test]
async fn the_report_names_each_flag_by_the_rewritten_event_id() -> TestResult {
    let (stub, _) = model().await?;
    let sanitized = advisory(format!("http://{}", stub.addr)).await?;
    let ids = event_ids(&sanitized.sealed.bundle);
    let markdown = sanitized.report.markdown("s.clean.json", "none");
    assert!(markdown.contains("Unreviewed: 3 of 3"), "{markdown}");
    assert!(markdown.contains("This review is advisory."), "{markdown}");
    assert!(
        markdown.contains(&format!(
            "### Flag 2 (score 0.91)\n\n- Session: `claude-code:c1`\n- Id: `{}`\n- Also at: `{}`\n\nMark: open\n",
            place(&ids, 1, "tool_args.command"),
            place(&ids, 3, "tool_args.command")
        )),
        "{markdown}"
    );
    assert!(
        !markdown.contains("evt-"),
        "flag ids name the rewritten event ids, never the raw ones: {markdown}"
    );
    stub.close().await?;
    Ok(())
}

#[tokio::test]
async fn top_caps_the_list_and_the_threshold_is_a_floor() -> TestResult {
    let (stub, _) = model().await?;
    let url = format!("http://{}", stub.addr);
    let one = decided(url.clone(), review(1, Mode::Advisory)?).await?;
    let numbers: Vec<(usize, String)> = one
        .report
        .decision
        .iter()
        .flat_map(|decision| &decision.flags)
        .map(|flag| (flag.number, flag.text.clone()))
        .collect();
    assert_eq!(numbers, [(1, risky_line())], "ties keep session order");

    let high = decided(
        url,
        Review {
            threshold: 0.6,
            ..review(20, Mode::Advisory)?
        },
    )
    .await?;
    assert_eq!(
        high.report.decision.map(|decision| decision.flags.len()),
        Some(2),
        "a chunk under the threshold is never listed"
    );
    stub.close().await?;
    Ok(())
}

#[tokio::test]
async fn block_mode_withholds_each_flag_until_it_is_marked() -> TestResult {
    let (stub, _) = model().await?;
    let sanitized = blocking(format!("http://{}", stub.addr)).await?;
    let bundle = &sanitized.sealed.bundle;
    let text = String::from_utf8(sanitized.sealed.text.clone())?;
    assert!(
        !text.contains(RISKY),
        "the bundle never carries flagged text"
    );
    assert_eq!(
        contents(bundle),
        ["[FLAGGED:1]", "running it", "running it", "running it"]
    );
    assert_eq!(command(bundle, 1).as_deref(), Some("[FLAGGED:2]"));
    assert_eq!(command(bundle, 3).as_deref(), Some("[FLAGGED:2]"));
    assert_eq!(
        (sanitize::withheld(bundle), stamp_flags(bundle)),
        (3, Some(3))
    );
    assert!(
        sanitized
            .report
            .markdown("s.clean.json", "none")
            .contains("This review blocks.")
    );
    stub.close().await?;
    Ok(())
}

#[tokio::test]
async fn an_unreachable_endpoint_fails_the_run() -> TestResult {
    let closed = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let result = advisory(format!("http://{closed}")).await;
    let error = result.err().ok_or("the run succeeded without the model")?;
    assert!(
        matches!(
            error.downcast_ref::<DecideError>(),
            Some(DecideError::Unreachable { .. })
        ),
        "{error}"
    );
    Ok(())
}

#[tokio::test]
async fn a_model_error_fails_the_run() -> TestResult {
    let routes = axum::Router::new()
        .route(
            "/v1/decide",
            axum::routing::post(|| async { (axum::http::StatusCode::SERVICE_UNAVAILABLE, "busy") }),
        )
        .route("/v1/decide/info", axum::routing::get(info));
    let stub = testkit::stub_server(routes).await?;
    let error = advisory(format!("http://{}", stub.addr))
        .await
        .err()
        .ok_or("the run succeeded on a 503")?;
    assert_eq!(
        error.to_string(),
        format!(
            "the decision model at http://{}/v1/decide answered HTTP 503: busy",
            stub.addr
        )
    );
    stub.close().await?;
    Ok(())
}

fn mark(report: &str, number: usize, mark: &str) -> String {
    let heading = format!("### Flag {number} ");
    let start = report.find(&heading).unwrap_or(report.len());
    let line = report[start..].find("Mark: open").map(|at| start + at);
    match line {
        Some(at) => format!(
            "{}Mark: {mark}{}",
            &report[..at],
            &report[at + "Mark: open".len()..]
        ),
        None => report.to_owned(),
    }
}

#[tokio::test]
async fn resolve_rewrites_an_advisory_bundle_in_place_and_converges() -> TestResult {
    let (stub, _) = model().await?;
    let sanitized = advisory(format!("http://{}", stub.addr)).await?;
    let local = sanitized.local.clone().ok_or("no local review")?;
    let bundle = sanitized.sealed.bundle;
    let report = sanitized.report.markdown("s.clean.json", "none");
    let report = mark(&report, 1, &format!("map {RISKY} as <customer>"));
    let report = mark(&report, 2, "**redact**");
    let report = mark(&report, 3, "keep");

    let flags = sanitize::marks::read_flags(&report)?;
    let resolved = sanitize::resolve(&bundle, &flags, &local, &policy()?.key)?;

    let clean = &resolved.sealed.bundle;
    assert_eq!(
        contents(clean),
        [
            "Ran the tests.\n<customer> is churning next quarter.",
            "running it",
            "running it",
            "running it"
        ]
    );
    assert_eq!(command(clean, 1).as_deref(), Some("[REDACTED:flagged]"));
    assert_eq!(
        command(clean, 3).as_deref(),
        Some("[REDACTED:flagged]"),
        "a mark applies at every place the flag lists"
    );
    assert_eq!((resolved.open, stamp_flags(clean)), (0, Some(0)));

    let applied: Vec<usize> = resolved.applied.iter().map(|(number, _)| *number).collect();
    let rewritten = flags.rewrite(&report, resolved.open, resolved.total, &applied);
    assert!(rewritten.contains(&format!("Resolved: map `{RISKY}` as `<customer>`")));
    assert!(rewritten.contains("Resolved: redact"));
    assert!(rewritten.contains("Resolved: keep"));
    assert!(rewritten.contains("Unreviewed: 0 of 3"));

    let again = sanitize::resolve(
        clean,
        &sanitize::marks::read_flags(&rewritten)?,
        &local,
        &policy()?.key,
    )?;
    assert_eq!(again.open, 0);
    assert_eq!(&again.sealed.bundle, clean);
    stub.close().await?;
    Ok(())
}

#[tokio::test]
async fn keep_puts_withheld_text_back_and_open_flags_stay_withheld() -> TestResult {
    let (stub, _) = model().await?;
    let sanitized = blocking(format!("http://{}", stub.addr)).await?;
    let report = mark(
        &sanitized.report.markdown("s.clean.json", "none"),
        1,
        "keep",
    );

    let flags = sanitize::marks::read_flags(&report)?;
    let resolved = sanitize::resolve(
        &sanitized.sealed.bundle,
        &flags,
        sanitized.local.as_ref().ok_or("no local review")?,
        &policy()?.key,
    )?;

    let clean = &resolved.sealed.bundle;
    assert_eq!(
        contents(clean).first().copied(),
        Some(risky_line().as_str())
    );
    assert_eq!(command(clean, 1).as_deref(), Some("[FLAGGED:2]"));
    assert_eq!(
        (resolved.open, stamp_flags(clean), sanitize::withheld(clean)),
        (2, Some(2), 2)
    );
    assert!(
        flags
            .rewrite(&report, resolved.open, resolved.total, &[1])
            .contains("Unreviewed: 2 of 3")
    );
    stub.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_bad_mark_names_its_flag() -> TestResult {
    let (stub, _) = model().await?;
    let sanitized = advisory(format!("http://{}", stub.addr)).await?;
    let report = sanitized.report.markdown("s.clean.json", "none");

    assert_eq!(
        sanitize::marks::read_flags(&mark(&report, 2, "maybe")),
        Err(ReportError::Mark {
            number: 2,
            problem: MarkProblem::Unknown("maybe".to_owned())
        })
    );
    assert_eq!(
        sanitize::marks::read_flags(&mark(&report, 1, "map Globex as <customer>")),
        Err(ReportError::Mark {
            number: 1,
            problem: MarkProblem::TermMissing("Globex".to_owned())
        })
    );
    stub.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_report_from_another_run_is_refused() -> TestResult {
    let (stub, _) = model().await?;
    let url = format!("http://{}", stub.addr);
    let first = advisory(url.clone()).await?;
    let policy = policy()?;
    let mut scrubbed = sanitize::scrub(&bundle()?, &policy)?;
    let candidates = scrubbed.candidates();
    let scores = sanitize::decide::score(&endpoint(url), &candidates.chunks).await?;
    scrubbed.flag(candidates, &scores, review(20, Mode::Advisory)?)?;
    let second = scrubbed.seal(&policy, DateTime::UNIX_EPOCH)?;

    let flags = sanitize::marks::read_flags(&first.report.markdown("s.clean.json", "none"))?;
    let error = sanitize::resolve(
        &second.sealed.bundle,
        &flags,
        second.local.as_ref().ok_or("no local review")?,
        &policy.key,
    )
    .err();

    assert!(
        matches!(error, Some(sanitize::SanitizeError::OtherRun { .. })),
        "{error:?}"
    );
    stub.close().await?;
    Ok(())
}
