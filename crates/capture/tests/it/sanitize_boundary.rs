use std::collections::BTreeMap;

use capture::sanitize;
use capture::sanitize::LocalKey;
use capture::sanitize::Policy;
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

const PASSWORD: &str = "hunter2PLANTEDq8RmT2v";
const KEY_VALUE: &str = "PLANTEDkeyLx9WcN4bKp7Hy";

fn at() -> Result<DateTime<Utc>, chrono::ParseError> {
    Ok(DateTime::parse_from_rfc3339("2026-11-09T12:00:00Z")?.with_timezone(&Utc))
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

fn session(key: &str, messages: Vec<CaptureMessage>) -> BundledSession {
    BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: "desk".to_owned(),
            conversation_id: format!("conv-{key}"),
            session_key: key.to_owned(),
            agent: "rivet".to_owned(),
            created_at: None,
            updated_at: None,
            identity: None,
        },
        batch: CaptureBatchRequest {
            session_key: key.to_owned(),
            agent: "rivet".to_owned(),
            channel: None,
            title: None,
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages,
        },
    }
}

fn raw(sessions: Vec<BundledSession>) -> Result<SessionBundle, Box<dyn std::error::Error>> {
    Ok(capture::bundle::seal(
        BundleAuthor {
            user: "phil".to_owned(),
            node: "desk".to_owned(),
        },
        at()?,
        Sanitization::Raw,
        sessions,
    )?
    .bundle)
}

fn policy(mapping: &str) -> Result<Policy, Box<dyn std::error::Error>> {
    keyed(mapping, [7; 32])
}

fn keyed(mapping: &str, key: [u8; 32]) -> Result<Policy, Box<dyn std::error::Error>> {
    Ok(Policy {
        mapping: sanitize::mapping::parse(mapping)?,
        key: LocalKey::from_bytes(key),
    })
}

fn sanitized_text(
    bundle: &SessionBundle,
    mapping: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let out = sanitize::run(bundle, &policy(mapping)?, at()?)?;
    Ok(String::from_utf8(out.sealed.text)?)
}

fn first_content(text: &str) -> Result<String, Box<dyn std::error::Error>> {
    let bundle = capture::bundle::parse(text)?;
    let session = bundle.sessions.first().ok_or("no session")?;
    let message = session.batch.messages.first().ok_or("no message")?;
    Ok(message.content.clone())
}

fn first_id(out: &sanitize::Sanitized) -> Result<String, Box<dyn std::error::Error>> {
    let bundle = capture::bundle::parse(std::str::from_utf8(&out.sealed.text)?)?;
    Ok(bundle
        .sessions
        .first()
        .and_then(|session| session.batch.messages.first())
        .map(|message| message.event_id.clone())
        .ok_or("no message")?)
}

#[test]
fn event_ids_are_keyed_hmacs_of_the_raw_id_and_session() -> TestResult {
    let secret = format!("deploy with password {PASSWORD}");
    let digest = capture::event_id::digest_text(&format!("assistant\0{secret}\0\0"));
    let original_id = format!("sess:occ:{digest}:0");
    let input = raw(vec![session("sess", vec![message(&original_id, &secret)])])?;
    let first = sanitize::run(&input, &policy("")?, at()?)?;
    let again = sanitize::run(&input, &policy("")?, at()?)?;
    let id = first_id(&first)?;
    assert_ne!(id, original_id);
    assert!(
        !id.contains(&digest),
        "the original digest is visible: {id}"
    );
    assert_eq!(
        first.sealed.text, again.sealed.text,
        "same raw bundle and key, same ids"
    );
    let other_key = sanitize::run(&input, &keyed("", [8; 32])?, at()?)?;
    assert_ne!(
        id,
        first_id(&other_key)?,
        "another machine's key, other ids"
    );
    let twin = raw(vec![session(
        "sess-b",
        vec![message(&original_id, &secret)],
    )])?;
    assert_ne!(
        id,
        first_id(&sanitize::run(&twin, &policy("")?, at()?)?)?,
        "the same raw event id in another session never merges"
    );
    Ok(())
}

#[test]
fn the_policy_hash_depends_on_the_raw_bundle() -> TestResult {
    let mapping = "[[rule]]\nliteral = \"Dana\"\naction = \"redact\"\n";
    let one = raw(vec![session("a", vec![message("e1", "Dana was here")])])?;
    let two = raw(vec![session("b", vec![message("e1", "Dana was there")])])?;
    let stamp = |bundle: &SessionBundle| -> Result<String, Box<dyn std::error::Error>> {
        let out = sanitize::run(bundle, &policy(mapping)?, at()?)?;
        match out.sealed.bundle.manifest.sanitization {
            Sanitization::Sanitized(stamp) => Ok(stamp.policy_sha256),
            Sanitization::Raw => Err("not stamped".into()),
        }
    };
    assert_ne!(stamp(&one)?, stamp(&two)?);
    assert_eq!(stamp(&one)?, stamp(&one)?);
    let other_key = sanitize::run(&one, &keyed(mapping, [9; 32])?, at()?)?;
    match other_key.sealed.bundle.manifest.sanitization {
        Sanitization::Sanitized(other) => assert_ne!(stamp(&one)?, other.policy_sha256),
        Sanitization::Raw => return Err("not stamped".into()),
    }
    Ok(())
}

#[test]
fn colliding_keys_keep_both_values_and_are_reported() -> TestResult {
    let mapping = concat!(
        "[[rule]]\nliteral = \"Matt\"\naction = \"redact\"\n\n",
        "[[rule]]\nliteral = \"matt\"\naction = \"redact\"\n"
    );
    let mut noted = message("e1", "x");
    noted.metadata = Some(serde_json::from_str(r#"{"Matt": 1, "matt": 2}"#)?);
    let input = raw(vec![session("col", vec![noted])])?;
    let out = sanitize::run(&input, &policy(mapping)?, at()?)?;
    let counts: BTreeMap<String, i64> = out
        .sealed
        .bundle
        .sessions
        .first()
        .and_then(|session| session.batch.messages.first())
        .and_then(|message| message.metadata.as_ref())
        .ok_or("no metadata")?
        .deserialize()?;
    assert_eq!(
        counts,
        BTreeMap::from([("[REDACTED]".to_owned(), 1), ("[REDACTED]#2".to_owned(), 2)])
    );
    let markdown = out.report.markdown("s.json", "m.toml");
    assert!(markdown.contains("`[REDACTED]#2`"), "{markdown}");
    Ok(())
}

#[test]
fn the_report_shows_scrubbed_key_paths_only() -> TestResult {
    let mut tool = message("e1", "x");
    tool.tool_args = Some(serde_json::from_str(&format!(
        r#"{{"client_secret={KEY_VALUE}": "x", "env": {{"API_TOKEN": "{PASSWORD}"}}}}"#
    ))?);
    let input = raw(vec![session("paths", vec![tool])])?;
    let out = sanitize::run(&input, &policy("")?, at()?)?;
    let markdown = out.report.markdown("s.json", "none");
    assert!(!markdown.contains(KEY_VALUE), "{markdown}");
    assert!(!markdown.contains(PASSWORD), "{markdown}");
    assert!(markdown.contains("tool_args.env.API_TOKEN"), "{markdown}");
    Ok(())
}

#[test]
fn a_mapped_secret_key_name_still_redacts_its_value() -> TestResult {
    let mapping = "[[rule]]\nliteral = \"password\"\naction = \"map\"\nas = \"<pw>\"\n";
    let mut tool = message("e1", "x");
    tool.tool_args = Some(serde_json::from_str(&format!(
        r#"{{"password": "{PASSWORD}", "cwd": "/home/x"}}"#
    ))?);
    let input = raw(vec![session("map", vec![tool])])?;
    let text = sanitized_text(&input, mapping)?;
    assert!(!text.contains(PASSWORD), "{text}");
    assert!(text.contains(r#""<pw>":"[REDACTED:secret_key]""#), "{text}");
    Ok(())
}

#[test]
fn a_secret_assignment_keeps_its_name_and_separator() -> TestResult {
    let input = raw(vec![session(
        "sep",
        vec![message(
            "e1",
            &format!("api_key: {KEY_VALUE} then DB_PASS=\"{PASSWORD}\" ok"),
        )],
    )])?;
    assert_eq!(
        first_content(&sanitized_text(&input, "")?)?,
        "api_key: [REDACTED:secret_assignment] then DB_PASS=\"[REDACTED:secret_assignment]\" ok"
    );
    Ok(())
}

#[test]
fn code_and_templates_are_not_secrets() -> TestResult {
    let kept = [
        "const API_KEY: &str = \"x\";",
        "ChangeKind::Secret => totals.secrets += 1,",
        "name: task-runner-worker-pool-default",
        "tags: disk_usage_percentage_report",
        "psql postgres://app:{PG_PASSWORD}@{HOST}/app",
        "postgres://user:<password>@host/db",
        "export DB_PASSWORD=${DB_PASSWORD}",
        "password: ****",
        "export API_TOKEN=.... Both run after",
        "Basic authentication is fine, Digest too.",
        "Authorization: Bearer",
        "{\"token\": \"{TOKEN_VALUE}\"}",
    ];
    for text in kept {
        let input = raw(vec![session("fp", vec![message("e1", text)])])?;
        assert_eq!(first_content(&sanitized_text(&input, "")?)?, text);
    }
    Ok(())
}

#[test]
fn a_word_boundary_in_a_mapping_regex_is_a_unicode_boundary() -> TestResult {
    let mapping = r#"
[[rule]]
regex = '\bJosé\b'
action = "map"
as = "<person>"

[[rule]]
regex = '\bLuis\b'
action = "map"
as = "<luis>"
"#;
    let cases = [
        ("ask José, not Joséphine", "ask <person>, not Joséphine"),
        ("Luis and Luis_x", "<luis> and Luis_x"),
        ("Luis, éLuis and Luisé", "<luis>, éLuis and Luisé"),
    ];
    for (text, expected) in cases {
        let input = raw(vec![session("uni", vec![message("e1", text)])])?;
        assert_eq!(first_content(&sanitized_text(&input, mapping)?)?, expected);
    }
    Ok(())
}

#[test]
fn crafted_long_lines_sanitize_in_bounded_time() -> TestResult {
    let pair = r#"{"name":"DB_PASSWORD","value":"#;
    let cases = [
        pair.repeat(160_000 / pair.len()),
        "password:>".repeat(75_000),
        "password=abc1 ".repeat(25_000),
    ];
    let mut outputs = Vec::new();
    for content in cases {
        let input = raw(vec![session("long", vec![message("e1", &content)])])?;
        let started = std::time::Instant::now();
        let text = sanitized_text(&input, "")?;
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "{} bytes took {elapsed:?}",
            content.len()
        );
        outputs.push(first_content(&text)?);
    }
    let pairs = outputs.first().ok_or("no output")?;
    assert!(pairs.len() < 1_000, "{pairs}");
    Ok(())
}

#[test]
fn a_flag_written_in_the_session_is_escaped_so_ingest_never_counts_it_withheld() -> TestResult {
    let bundle = raw(vec![session(
        "claude-code:s",
        vec![message(
            "e1",
            "the test expects a [FLAGGED:12] b [FLAGGED:x]",
        )],
    )])?;
    let out = sanitize::run(&bundle, &policy("")?, at()?)?;
    let text = String::from_utf8(out.sealed.text.clone())?;
    assert_eq!(
        first_content(&text)?,
        "the test expects a [FLAGGED 12] b [FLAGGED:x]"
    );
    assert_eq!(sanitize::withheld(&out.sealed.bundle), 0);
    assert_eq!(
        out.report
            .changes
            .iter()
            .map(|change| change.rule.to_string())
            .collect::<Vec<_>>(),
        ["[FLAGGED:<n>] written in the session"]
    );
    Ok(())
}

#[test]
fn ingest_scan_accepts_the_flag_forms_after_secret_names() -> TestResult {
    let mut tool = message(
        "e2",
        "DB_PASSWORD=[FLAGGED:3]\nexport API_TOKEN=\"[FLAGGED:12]\"\napi_token: [REDACTED:flagged]",
    );
    tool.tool_args = Some(serde_json::from_str(
        r#"{"password": "[FLAGGED:4]", "token": "[REDACTED:flagged]"}"#,
    )?);
    let flagged = raw(vec![session("claude-code:s", vec![tool])])?;
    assert_eq!(
        sanitize::secrets(&flagged)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        Vec::<String>::new()
    );
    let lookalike = raw(vec![session(
        "claude-code:s",
        vec![message("e3", "DB_PASSWORD=[FLAGGED:x]")],
    )])?;
    assert_eq!(sanitize::secrets(&lookalike).len(), 1);
    Ok(())
}
