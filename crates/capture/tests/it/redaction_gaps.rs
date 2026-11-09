use std::sync::Arc;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use config::Environment;
use parking_lot::Mutex;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::CaptureResult;
use wire::message::Role;

const AWS_KEY: &str = concat!("AKIA", "ABCDEFGHIJKLMNOP");

fn builtins() -> Result<capture::redaction::Redaction, &'static str> {
    capture::redaction::resolve(
        Some(config::RedactionSettings {
            enabled: true,
            builtins: true,
            patterns: Vec::new(),
        }),
        &Environment::default(),
    )
    .ok_or("builtin redaction should resolve")
}

fn message(content: &str) -> CaptureMessage {
    CaptureMessage {
        event_id: "e1".to_owned(),
        role: Role::User,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    }
}

fn redacted_content(content: &str) -> Result<(String, u64), &'static str> {
    let mut message = message(content);
    let spans = builtins()?.redact_message(&mut message);
    Ok((message.content, spans))
}

fn text_at<'a>(object: Option<&'a wire::JsonObject>, key: &str) -> Option<&'a str> {
    object
        .and_then(|object| object.get(key))
        .and_then(|value| value.as_str())
}

#[test]
fn tool_arg_keys_containing_a_secret_word_are_replaced() -> Result<(), Box<dyn std::error::Error>> {
    let secret_keys = [
        "access_token",
        "auth_token",
        "client_secret",
        "Authorization",
        "X-Api-Key",
        "db_password",
        "aws_secret_access_key",
        "SESSION_KEY",
        "Cookie",
        "pwd",
        "auth",
    ];
    let ordinary_keys = ["command", "file_path", "description", "query", "url"];
    let redaction = builtins()?;
    for key in secret_keys {
        let mut message = message("");
        message.tool_args = Some(serde_json::from_str(&format!(
            r#"{{"{key}":"opaque-value"}}"#
        ))?);
        redaction.redact_message(&mut message);
        assert_eq!(
            text_at(message.tool_args.as_ref(), key),
            Some("[REDACTED:secret_key]"),
            "tool_args key {key} kept its value"
        );
    }
    for key in ordinary_keys {
        let mut message = message("");
        message.tool_args = Some(serde_json::from_str(&format!(
            r#"{{"{key}":"opaque-value"}}"#
        ))?);
        redaction.redact_message(&mut message);
        assert_eq!(
            text_at(message.tool_args.as_ref(), key),
            Some("opaque-value"),
            "tool_args key {key} lost its value"
        );
    }
    Ok(())
}

#[test]
fn http_auth_scheme_credentials_are_redacted() -> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        (
            concat!("Authorization: Basic ", "dXNlcjpwYXNz"),
            "dXNlcjpwYXNz",
        ),
        (
            concat!("token: Negotiate ", "YIIBhwYGKwYBBQUC"),
            "YIIBhwYGKwYBBQUC",
        ),
        (
            concat!(r#"curl -H "Authorization: Digest "#, r#"dXNlcjpwYXNz" x"#),
            "dXNlcjpwYXNz",
        ),
        (
            concat!("NTLM ", "TlRMTVNTUAABAAAAB4IIog=="),
            "TlRMTVNTUAABAAAAB4IIog==",
        ),
    ];
    for (raw, credential) in cases {
        let (redacted, spans) = redacted_content(raw)?;
        assert!(
            !redacted.contains(credential),
            "credential survived: {redacted}"
        );
        assert!(spans > 0, "no span counted for {raw}");
    }
    for prose in [
        "Basic authentication is fine, Digest too.",
        "We will Negotiate terms.",
    ] {
        assert_eq!(redacted_content(prose)?, (prose.to_owned(), 0));
    }
    Ok(())
}

#[test]
fn json_quoted_assignments_are_redacted() -> Result<(), Box<dyn std::error::Error>> {
    let value = "abcdefghijklmnop1234567890";
    let shapes = [
        format!(r#""api_key": "{value}""#),
        format!(r#""api_key":"{value}""#),
        format!(r#"{{"token": "{value}"}}"#),
        format!(r#"{{"auth_token": "{value}"}}"#),
        format!("{{'api_key': '{value}'}}"),
    ];
    for shape in shapes {
        let (redacted, spans) = redacted_content(&shape)?;
        assert!(!redacted.contains(value), "value survived: {redacted}");
        assert!(
            redacted.contains("[REDACTED:secret_assignment]"),
            "{redacted}"
        );
        assert_eq!(spans, 1);
    }
    assert_eq!(
        redacted_content("api_key: hunter2 next")?.0,
        "api_key: [REDACTED:secret_assignment] next"
    );
    Ok(())
}

#[test]
fn title_settings_and_metadata_are_scanned() -> Result<(), Box<dyn std::error::Error>> {
    let github = concat!("ghp_", "abcdefghijklmnopqrstuvwx0123456789");
    let mut batch: CaptureBatchRequest = serde_json::from_str(&format!(
        r#"{{"session_key":"s","agent":"a","title":"deploy with {AWS_KEY}",
        "settings":{{"cwd":"/work","api_token":"opaque","note":"Bearer abcdefghijklmnop1234"}},
        "messages":[{{"event_id":"e1","role":"user","content":"hi",
        "metadata":{{"note":"{github}","ingest_key":"k-1","client_secret":"x"}}}}]}}"#
    ))?;
    let spans = builtins()?.redact_batch(&mut batch);
    assert_eq!(spans, 3);
    assert_eq!(
        batch.title.as_deref(),
        Some("deploy with [REDACTED:aws_access_key]")
    );
    let settings = batch.settings.as_ref();
    assert_eq!(text_at(settings, "cwd"), Some("/work"));
    assert_eq!(
        text_at(settings, "api_token"),
        Some("[REDACTED:secret_key]")
    );
    assert_eq!(text_at(settings, "note"), Some("Bearer [REDACTED:bearer]"));
    let metadata = batch.messages.first().and_then(|m| m.metadata.as_ref());
    assert_eq!(text_at(metadata, "note"), Some("[REDACTED:github_token]"));
    assert_eq!(text_at(metadata, "ingest_key"), Some("k-1"));
    assert_eq!(
        text_at(metadata, "client_secret"),
        Some("[REDACTED:secret_key]")
    );
    Ok(())
}

#[derive(Clone, Default)]
struct RecordingTransport {
    posts: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl capture::writer::CaptureTransport for RecordingTransport {
    async fn post(&self, body: String) -> Result<CaptureResult, capture::writer::PostError> {
        self.posts.lock().push(body);
        Ok(CaptureResult {
            ok: true,
            conversation_id: "c-1".to_owned(),
            inserted: 1,
            skipped: 0,
        })
    }
}

async fn posted_message(
    batch: CaptureBatchRequest,
) -> Result<CaptureMessage, Box<dyn std::error::Error>> {
    let transport = RecordingTransport::default();
    let spool_dir = tempfile::tempdir()?;
    let writer = capture::writer::CaptureWriter::new(
        transport.clone(),
        capture::spool::CaptureSpool::new(spool_dir.path().to_path_buf()),
        None,
        Some(builtins()?),
    );
    let now: DateTime<Utc> = "2026-01-01T00:00:00Z".parse()?;
    writer.write(&batch, now).await?;
    let body = transport
        .posts
        .lock()
        .first()
        .cloned()
        .ok_or("nothing was posted")?;
    let posted: CaptureBatchRequest = serde_json::from_str(&body)?;
    Ok(posted
        .messages
        .into_iter()
        .next()
        .ok_or("no message was posted")?)
}

fn batch_of(message: CaptureMessage) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: "s".to_owned(),
        agent: "a".to_owned(),
        channel: None,
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![message],
    }
}

#[tokio::test]
async fn no_unscanned_text_reaches_the_capped_content() -> Result<(), Box<dyn std::error::Error>> {
    let bearer = format!("Bearer {}", "ab".repeat(4500));
    let filler = "x".repeat(16_000 - bearer.chars().count() - 1);
    let shrunk = format!("{bearer} {filler}{AWS_KEY}{}", "y".repeat(100));
    let posted = posted_message(batch_of(message(&shrunk))).await?;
    assert!(
        !posted.content.contains("AKIA"),
        "a secret past the scanned head was posted"
    );
    assert_eq!(
        posted.content.chars().count(),
        16_000 - bearer.chars().count() + 24 + 25 + 100
    );
    let straddling = format!("{}{AWS_KEY}{}", "x".repeat(15_990), "y".repeat(100));
    let posted = posted_message(batch_of(message(&straddling))).await?;
    assert!(
        !posted.content.contains("AKIA"),
        "the head of a straddling secret was posted"
    );
    assert_eq!(posted.content.chars().count(), 16_000);
    Ok(())
}

#[tokio::test]
async fn long_tool_arg_strings_are_scanned_to_the_end() -> Result<(), Box<dyn std::error::Error>> {
    let mut long = message("");
    long.tool_args = Some(serde_json::from_str(&format!(
        r#"{{"command":"{}{AWS_KEY}"}}"#,
        "x".repeat(20_000)
    ))?);
    let posted = posted_message(batch_of(long)).await?;
    let command = text_at(posted.tool_args.as_ref(), "command").ok_or("command is missing")?;
    assert!(command.ends_with("[REDACTED:aws_access_key]"));
    Ok(())
}
