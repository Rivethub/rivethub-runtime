use config::Environment;
use wire::capture::CaptureMessage;
use wire::message::Role;

fn redaction(
    enabled: bool,
    builtins: bool,
    patterns: &[&str],
) -> Option<capture::redaction::Redaction> {
    capture::redaction::resolve(
        Some(config::RedactionSettings {
            enabled,
            builtins,
            patterns: patterns.iter().map(|p| (*p).to_owned()).collect(),
        }),
        &Environment::default(),
    )
}

fn message(content: &str, tool_args: &str) -> Result<CaptureMessage, serde_json::Error> {
    Ok(CaptureMessage {
        event_id: "e1".to_owned(),
        role: Role::User,
        content: content.to_owned(),
        tool_name: None,
        tool_args: Some(serde_json::from_str(tool_args)?),
        tool_result: None,
        metadata: None,
        created_at: None,
    })
}

#[test]
fn redaction_is_off_unless_enabled() {
    assert!(redaction(false, true, &[]).is_none());
    assert!(capture::redaction::resolve(None, &Environment::default()).is_none());
}

#[test]
fn environment_opt_in_enables_builtins() {
    for value in ["1", "true", "yes", "on", "TRUE"] {
        let env = Environment::from_vars([(
            capture::redaction::ENV_VARIABLE.to_owned(),
            value.to_owned(),
        )]);
        assert!(
            capture::redaction::resolve(None, &env).is_some(),
            "value {value} should opt in"
        );
    }
    let off =
        Environment::from_vars([(capture::redaction::ENV_VARIABLE.to_owned(), "0".to_owned())]);
    assert!(capture::redaction::resolve(None, &off).is_none());
}

#[test]
fn builtins_off_with_no_patterns_resolves_to_none() {
    assert!(redaction(true, false, &[]).is_none());
    assert!(redaction(true, false, &["SECRET[0-9]+"]).is_some());
    assert!(redaction(true, true, &[]).is_some());
}

#[test]
fn unsafe_and_uncompilable_patterns_are_skipped() -> Result<(), Box<dyn std::error::Error>> {
    let redaction =
        redaction(true, false, &["(a+)+b", "[unclosed", "SECRET[0-9]+"]).expect("resolves");
    let mut message = message("x=SECRET42", "{}")?;
    let spans = redaction.redact_message(&mut message);
    assert_eq!(spans, 1);
    assert_eq!(message.content, "x=[REDACTED:pattern:0]");
    Ok(())
}

#[test]
fn builtins_replace_secret_spans() -> Result<(), Box<dyn std::error::Error>> {
    let redaction = redaction(true, true, &[]).expect("resolves");
    let mut message = message(
        concat!(
            "Bearer abcdefghijklmnop1234 and ",
            "AKIA",
            "ABCDEFGHIJKLMNOP and pwd=hunter2"
        ),
        concat!(
            r#"{"api_key":"sk-"#,
            r#"12345678901234567890","#,
            r#""nested":{"password":"p","ok":"fine"}}"#
        ),
    )?;
    let spans = redaction.redact_message(&mut message);
    assert!(spans >= 3);
    assert!(message.content.contains("[REDACTED:bearer]"));
    assert!(message.content.contains("[REDACTED:aws_access_key]"));
    assert!(message.content.contains("pwd=[REDACTED:secret_assignment]"));
    let args = message.tool_args.expect("args");
    assert_eq!(
        args.get("api_key").and_then(|v| v.as_str()),
        Some("[REDACTED:secret_key]")
    );
    let nested = args.get("nested").and_then(|v| v.as_object());
    assert_eq!(
        nested
            .and_then(|n| n.get("password"))
            .and_then(|v| v.as_str()),
        Some("[REDACTED:secret_key]")
    );
    assert_eq!(
        nested.and_then(|n| n.get("ok")).and_then(|v| v.as_str()),
        Some("fine")
    );
    Ok(())
}

#[test]
fn scan_covers_only_the_head_of_long_text() -> Result<(), Box<dyn std::error::Error>> {
    let redaction = redaction(true, true, &[]).expect("resolves");
    let tail_secret = "Bearer abcdefghijklmnop1234";
    let content = "x".repeat(30000) + tail_secret;
    let mut tail_case = message(&content, "{}")?;
    let spans = redaction.redact_message(&mut tail_case);
    assert_eq!(spans, 0);
    assert!(tail_case.content.ends_with(tail_secret));
    let head_secret = format!("{tail_secret}{}", "y".repeat(17000));
    let mut head_case = message(&head_secret, "{}")?;
    let spans = redaction.redact_message(&mut head_case);
    assert_eq!(spans, 1);
    assert!(head_case.content.starts_with("Bearer [REDACTED:bearer]"));
    Ok(())
}

#[derive(Default)]
struct KeptBodies {
    bodies: parking_lot::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl capture::writer::CaptureTransport for &KeptBodies {
    async fn post(
        &self,
        body: String,
    ) -> Result<wire::capture::CaptureResult, capture::writer::PostError> {
        self.bodies.lock().push(body);
        Ok(wire::capture::CaptureResult {
            ok: true,
            conversation_id: "c-1".to_owned(),
            inserted: 1,
            skipped: 0,
        })
    }
}

#[tokio::test]
async fn the_writer_redacts_the_session_title() -> Result<(), Box<dyn std::error::Error>> {
    let redaction = redaction(true, true, &[]).ok_or("redaction should resolve")?;
    let transport = KeptBodies::default();
    let dir = tempfile::tempdir()?;
    let writer = capture::writer::CaptureWriter::new(
        &transport,
        capture::spool::CaptureSpool::new(dir.path().to_path_buf()),
        None,
        Some(redaction),
    );
    let mut batch = capture::batch::parse_batch(
        r#"{"session_key":"s","agent":"a","messages":[{"event_id":"e1","role":"user","content":"hi"}]}"#,
    )?;
    batch.title = Some("deploy with Bearer abcdefghijklmnop1234 now".to_owned());
    writer
        .write(&batch, "2026-10-10T12:00:00Z".parse()?)
        .await?;
    let bodies = transport.bodies.lock().clone();
    let [body] = bodies.as_slice() else {
        return Err("expected one capture post".into());
    };
    let posted = capture::batch::parse_batch(body)?;
    assert_eq!(
        posted.title.as_deref(),
        Some("deploy with Bearer [REDACTED:bearer] now")
    );
    Ok(())
}
