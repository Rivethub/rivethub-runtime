use capture::sanitize;
use capture::sanitize::LocalKey;
use capture::sanitize::Policy;
use capture::sanitize::SanitizeError;
use capture::sanitize::mapping::MappingError;
use capture::sanitize::mapping::RuleProblem;
use chrono::DateTime;
use chrono::Utc;
use wire::JsonObject;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::Sanitization;
use wire::capture::SanitizeStamp;
use wire::capture::SessionBundle;
use wire::capture::SessionOrigin;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const GITHUB: &str = concat!("ghp_", "Zq8RmT2vLx9WcN4bKp7HyJ3dFs6GtA1eUo5V");
const AWS: &str = concat!("AKIA", "QW3RT5YU7IO9PA2S");
const JWT: &str = concat!(
    "eyJhbGciOiJIUzI1NiJ9",
    ".eyJzdWIiOiJwbGFudGVkIn0",
    ".c2lnbmF0dXJlLXBsYW50ZWQ"
);
const PEM_OPEN: &str = concat!("-----BEGIN ", "PRIVATE KEY-----");
const PEM_CLOSE: &str = concat!("-----END ", "PRIVATE KEY-----");
const PEM_BODY: &str = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCplanted";
const PG_PASSWORD: &str = "s3cr3t-pg-pass";
const ENV_PASSWORD: &str = "envpass-xyz-42";
const API_KEY: &str = "apikeyvalue-planted-123";
const TOKEN_VALUE: &str = "plain-token-value-77";
const HOST: &str = "192.0.2.77";
const TEAMMATE: &str = "Alice Example";
const CODENAME: &str = "Project Nightjar";
const NODE: &str = "buildbox-7";

const PLANTED: [&str; 12] = [
    GITHUB,
    AWS,
    JWT,
    PEM_BODY,
    PG_PASSWORD,
    ENV_PASSWORD,
    API_KEY,
    TOKEN_VALUE,
    HOST,
    TEAMMATE,
    CODENAME,
    NODE,
];

const MAPPING: &str = r#"
[[rule]]
regex = '192\.0\.2\.\d+'
action = "map"
as = "<buildhost>"

[[rule]]
literal = "Alice Example"
action = "map"
as = "<teammate>"

[[rule]]
literal = "Project Nightjar"
action = "redact"

[[rule]]
literal = "buildbox-7"
action = "map"
as = "<node>"
"#;

fn policy() -> Result<Policy, MappingError> {
    Ok(Policy {
        mapping: sanitize::mapping::parse(MAPPING)?,
        key: LocalKey::from_bytes([7; 32]),
    })
}

fn at() -> Result<DateTime<Utc>, chrono::ParseError> {
    Ok(DateTime::parse_from_rfc3339("2026-11-08T12:00:00Z")?.with_timezone(&Utc))
}

fn object(text: &str) -> Result<JsonObject, serde_json::Error> {
    serde_json::from_str(text)
}

fn message(event_id: &str, content: String) -> CaptureMessage {
    CaptureMessage {
        event_id: event_id.to_owned(),
        role: Role::Assistant,
        content,
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    }
}

fn session(
    conversation: &str,
    messages: Vec<CaptureMessage>,
) -> Result<BundledSession, serde_json::Error> {
    Ok(BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: NODE.to_owned(),
            conversation_id: conversation.to_owned(),
            session_key: format!("claude-code:{conversation}"),
            agent: "rivet".to_owned(),
            created_at: None,
            updated_at: None,
            identity: None,
        },
        batch: CaptureBatchRequest {
            session_key: format!("claude-code:{conversation}"),
            agent: "rivet".to_owned(),
            channel: None,
            title: Some(format!("deploy to {HOST} with {TEAMMATE}")),
            settings: Some(object(&format!(
                r#"{{"cwd": "/home/user/{CODENAME}", "hosts": {{"{HOST}": "up"}}}}"#
            ))?),
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages,
        },
    })
}

fn corpus() -> Result<BundledSession, Box<dyn std::error::Error>> {
    let mut tool = message("evt-2", "running the deploy".to_owned());
    tool.tool_name = Some("Bash".to_owned());
    tool.tool_args = Some(object(&format!(
        r#"{{"command": "psql postgres://app:{PG_PASSWORD}@{HOST}/app", "token": "{TOKEN_VALUE}", "env": ["DB_PASSWORD={ENV_PASSWORD}"]}}"#
    ))?);
    tool.tool_result = Some(format!(
        "{PEM_OPEN}\n{PEM_BODY}\n{PEM_CLOSE}\nexport DB_PASSWORD={ENV_PASSWORD}\napi_key = {API_KEY}\n"
    ));
    let mut noted = message(
        "evt-3",
        format!("Authorization: Bearer {JWT} and {AWS} for {TEAMMATE}"),
    );
    noted.metadata = Some(object(&format!(
        r#"{{"reviewer": "{TEAMMATE}", "push": "{GITHUB}"}}"#
    ))?);
    Ok(session(
        "0195f0c2-0000-7000-8000-000000000001",
        vec![
            message(
                "evt-1",
                format!("ask {TEAMMATE} about {CODENAME} on {HOST}"),
            ),
            tool,
            noted,
        ],
    )?)
}

fn raw(sessions: Vec<BundledSession>) -> Result<SessionBundle, Box<dyn std::error::Error>> {
    Ok(capture::bundle::seal(
        BundleAuthor {
            user: "phil".to_owned(),
            node: NODE.to_owned(),
        },
        at()?,
        Sanitization::Raw,
        sessions,
    )?
    .bundle)
}

fn text(bundle: &SessionBundle) -> Result<String, serde_json::Error> {
    serde_json::to_string(bundle)
}

#[test]
fn every_planted_secret_is_gone() -> TestResult {
    let input = raw(vec![corpus()?])?;
    let before = text(&input)?;
    for planted in PLANTED {
        assert!(before.contains(planted), "the corpus plants {planted}");
    }
    let sanitized = sanitize::run(&input, &policy()?, at()?)?;
    let file = String::from_utf8(sanitized.sealed.text.clone())?;
    for planted in PLANTED {
        assert!(!file.contains(planted), "{planted} survived sanitizing");
    }
    let parsed = capture::bundle::parse(&file)?;
    assert_eq!(parsed, sanitized.sealed.bundle);
    assert_eq!(sanitize::secrets(&parsed), Vec::new());
    assert_eq!(parsed.manifest.exported_by.user, "phil");
    assert_eq!(parsed.manifest.exported_by.node, "<node>");
    let first = first(&parsed)?;
    assert_eq!(first.origin.author, "phil");
    assert_eq!(first.origin.node, "<node>");
    let Sanitization::Sanitized(stamp) = &parsed.manifest.sanitization else {
        panic!("the output is stamped sanitized");
    };
    assert_eq!(
        stamp,
        &SanitizeStamp {
            at: at()?,
            policy_sha256: policy()?.hash(&input.manifest.bundle_sha256)?,
            passes: vec!["secrets".into(), "mapping".into()],
            unresolved_flags: 0,
        }
    );
    let settings = first.batch.settings.as_ref().map(text_of);
    assert_eq!(
        settings.as_deref(),
        Some(r#"{"cwd":"/home/user/[REDACTED]","hosts":{"<buildhost>":"up"}}"#)
    );
    Ok(())
}

fn first(bundle: &SessionBundle) -> Result<&BundledSession, &'static str> {
    bundle.sessions.first().ok_or("the bundle holds no session")
}

fn first_message(session: &BundledSession) -> Result<&CaptureMessage, &'static str> {
    session
        .batch
        .messages
        .first()
        .ok_or("the session holds no message")
}

fn text_of(object: &JsonObject) -> String {
    String::from_utf8(wire::json::canonical_json(object).unwrap_or_default()).unwrap_or_default()
}

#[test]
fn tool_results_are_kept_and_scrubbed() -> TestResult {
    let filler = "line of build output\n".repeat(2_000);
    let mut tool = message("evt-1", "build".to_owned());
    tool.tool_result = Some(format!("{filler}token = {TOKEN_VALUE}\n{filler}done"));
    let input = raw(vec![session(
        "0195f0c2-0000-7000-8000-000000000002",
        vec![tool],
    )?])?;
    let output = sanitize::run(&input, &policy()?, at()?)?.sealed.bundle;
    assert_eq!(
        first_message(first(&output)?)?.tool_result.as_deref(),
        Some(format!("{filler}token = [REDACTED:secret_assignment]\n{filler}done").as_str())
    );
    Ok(())
}

#[test]
fn mapping_is_stable_across_sessions() -> TestResult {
    let input = raw(vec![
        session(
            "0195f0c2-0000-7000-8000-000000000003",
            vec![message("evt-1", format!("ssh {HOST}"))],
        )?,
        session(
            "0195f0c2-0000-7000-8000-000000000004",
            vec![message("evt-9", format!("ping {HOST} twice: {HOST}"))],
        )?,
    ])?;
    let output = sanitize::run(&input, &policy()?, at()?)?.sealed.bundle;
    let contents = output
        .sessions
        .iter()
        .map(|session| first_message(session).map(|message| message.content.as_str()))
        .collect::<Result<Vec<&str>, _>>()?;
    assert_eq!(
        contents,
        ["ssh <buildhost>", "ping <buildhost> twice: <buildhost>"]
    );
    Ok(())
}

#[test]
fn the_report_never_contains_redacted_text() -> TestResult {
    let input = raw(vec![corpus()?])?;
    let report = sanitize::run(&input, &policy()?, at()?)?.report;
    let markdown = report.markdown("s.clean.json", "redactions.toml (4 rules)");
    let debug = format!("{report:?}");
    for planted in PLANTED {
        assert!(!markdown.contains(planted), "the report shows {planted}");
        assert!(!debug.contains(planted), "the report holds {planted}");
    }
    assert!(
        markdown.contains("| mapping | rule at line 12 (redact) | `[REDACTED]` | 2 |"),
        "{markdown}"
    );
    assert!(
        markdown.contains("| tool_args.token | secret key name | `[REDACTED:secret_key]` | 1 |"),
        "{markdown}"
    );
    assert!(
        markdown.contains(
            "| - | settings.hosts.<buildhost> (a key) | rule at line 2 (map) | `<buildhost>` | 1 |"
        ),
        "{markdown}"
    );
    let totals = report.totals();
    assert_eq!(
        (totals.secrets, totals.mapped, totals.redacted),
        (9, 10, 2),
        "{markdown}"
    );
    Ok(())
}

#[test]
fn a_sanitized_bundle_cannot_be_sanitized_again() -> TestResult {
    let once = sanitize::run(&raw(vec![corpus()?])?, &policy()?, at()?)?;
    let again = sanitize::run(&once.sealed.bundle, &policy()?, at()?);
    assert!(
        matches!(again, Err(SanitizeError::AlreadySanitized { at: stamped }) if stamped == at()?)
    );
    Ok(())
}

#[test]
fn a_forged_stamp_on_a_raw_bundle_still_shows_its_secrets() -> TestResult {
    let mut forged = raw(vec![corpus()?])?;
    forged.manifest.sanitization = Sanitization::Sanitized(SanitizeStamp {
        at: at()?,
        policy_sha256: "0".repeat(64),
        passes: vec!["secrets".into()],
        unresolved_flags: 0,
    });
    let found = sanitize::secrets(&forged);
    assert_eq!(found.len(), 9);
    assert_eq!(
        found.first().map(ToString::to_string).unwrap_or_default(),
        "session 1 message 2 tool_result: pem_private_key"
    );
    Ok(())
}

#[test]
fn sanitizing_is_deterministic() -> TestResult {
    let input = raw(vec![corpus()?])?;
    let first = sanitize::run(&input, &policy()?, at()?)?;
    let second = sanitize::run(&input, &policy()?, at()?)?;
    assert_eq!(first.sealed.text, second.sealed.text);
    assert_eq!(first.report, second.report);
    Ok(())
}

#[test]
fn a_bad_regex_names_its_line() {
    let parsed = sanitize::mapping::parse(
        "[[rule]]\nliteral = \"a\"\naction = \"redact\"\n\n[[rule]]\nregex = \"(unclosed\"\naction = \"redact\"\n",
    );
    assert!(
        matches!(&parsed, Err(MappingError::Regex { line: 5, .. })),
        "{parsed:?}"
    );
}

#[test]
fn a_rule_missing_its_placeholder_names_its_line() {
    let parsed = sanitize::mapping::parse("\n[[rule]]\nliteral = \"a\"\naction = \"map\"\n");
    assert_eq!(
        parsed.map(|rules| rules.rules().len()),
        Err(MappingError::Rule {
            line: 2,
            problem: RuleProblem::MapWithoutPlaceholder
        })
    );
}

#[test]
fn a_misspelled_rule_field_is_refused() {
    let parsed = sanitize::mapping::parse("[[rule]]\nlitteral = \"a\"\naction = \"redact\"\n");
    assert!(
        matches!(&parsed, Err(MappingError::Toml { line: 2, .. })),
        "{parsed:?}"
    );
}

#[test]
fn a_mapped_placeholder_that_reads_as_a_secret_is_redacted_before_ingest_sees_it() -> TestResult {
    let mapping = sanitize::mapping::parse(
        "[[rule]]\nliteral = \"Nightjar\"\naction = \"map\"\nas = \"db_password=mapped-pw-0001\"\n",
    )?;
    let input = raw(vec![session(
        "0195f0c2-0000-7000-8000-000000000003",
        vec![message("evt-1", "ship Nightjar today".to_owned())],
    )?])?;
    let sanitized = sanitize::run(
        &input,
        &Policy {
            mapping,
            key: LocalKey::from_bytes([7; 32]),
        },
        at()?,
    )?;
    let file = String::from_utf8(sanitized.sealed.text.clone())?;
    assert!(!file.contains("mapped-pw-0001"), "{file}");
    assert_eq!(sanitize::secrets(&sanitized.sealed.bundle), []);
    assert!(sanitized.report.rechecked > 0);
    assert!(
        sanitized
            .report
            .markdown("s.clean.json", "m.toml")
            .contains("re-check")
    );
    Ok(())
}

fn gzip_base64(bytes: &[u8]) -> Result<String, std::io::Error> {
    use base64::Engine as _;
    use std::io::Write as _;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(bytes)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(gz.finish()?))
}

#[test]
fn a_decode_bomb_is_redacted_whole_in_sanitize_and_ingest() -> TestResult {
    let inner = gzip_base64(&vec![b'A'; 4 << 20])?;
    let layer = [inner.as_str(); 12].join(" ");
    let bomb = gzip_base64(layer.as_bytes())?;
    let mut tool = message("evt-1", "x".to_owned());
    tool.tool_result = Some(format!("blob {bomb}"));
    let input = raw(vec![session(
        "0195f0c2-0000-7000-8000-000000000004",
        vec![tool],
    )?])?;
    assert!(
        sanitize::secrets(&input)
            .iter()
            .any(|change| change.rule.to_string() == "undecoded_blob")
    );
    let sanitized = sanitize::run(&input, &policy()?, at()?)?;
    let file = String::from_utf8(sanitized.sealed.text.clone())?;
    assert!(!file.contains(&bomb[..40]));
    assert!(file.contains("[REDACTED:undecoded_blob]"), "{file}");
    assert!(
        sanitized
            .report
            .markdown("s.clean.json", "m.toml")
            .contains("undecoded_blob")
    );
    Ok(())
}

#[test]
fn a_bomb_in_every_message_costs_the_bundle_budget_not_the_message_count() -> TestResult {
    let inner = gzip_base64(&vec![0xff; 1 << 20])?;
    let bomb = gzip_base64([inner.as_str(); 9].join(" ").as_bytes())?;
    let scan = |count: usize| -> Result<std::time::Duration, Box<dyn std::error::Error>> {
        let messages = (0..count)
            .map(|at| {
                let mut tool = message(&format!("evt-{at}"), "x".to_owned());
                tool.tool_result = Some(format!("blob {bomb}"));
                tool
            })
            .collect();
        let input = raw(vec![session(
            "0195f0c2-0000-7000-8000-000000000005",
            messages,
        )?])?;
        let started = std::time::Instant::now();
        let found = sanitize::secrets(&input);
        let took = started.elapsed();
        assert_eq!(found.len(), count);
        assert!(
            found
                .iter()
                .all(|change| change.rule.to_string() == "undecoded_blob")
        );
        Ok(took)
    };
    scan(1)?;
    let few = scan(16)?;
    let many = scan(1024)?;
    assert!(many < few * 8, "16 bombs took {few:?}, 1024 took {many:?}");
    Ok(())
}
