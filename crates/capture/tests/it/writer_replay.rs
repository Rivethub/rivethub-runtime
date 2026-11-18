use jiff::Timestamp;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use testkit::json_eq;
use wire::capture::CaptureResult;

fn recording(name: &str) -> String {
    testkit::recording("capture/writer")
        .join(name)
        .to_string_lossy()
        .to_string()
}

#[derive(Clone)]
struct RecordingTransport {
    posts: Arc<Mutex<Vec<String>>>,
}

impl RecordingTransport {
    fn new() -> Self {
        Self {
            posts: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl capture::writer::CaptureTransport for RecordingTransport {
    async fn post(&self, body: String) -> Result<CaptureResult, capture::writer::PostError> {
        let parsed: testkit::Json =
            serde_json::from_str(&body).map_err(|error| capture::writer::PostError::Network {
                message: format!("the writer posted a body that is not JSON: {error}"),
            })?;
        let messages = parsed
            .get("messages")
            .and_then(testkit::Json::as_array)
            .map(Vec::len)
            .ok_or_else(|| capture::writer::PostError::Network {
                message: "the posted batch body carries no messages array".to_owned(),
            })?;
        let response = CaptureResult {
            ok: true,
            conversation_id: "c-1".to_owned(),
            inserted: messages as u64,
            skipped: 0,
        };
        self.posts.lock().push(body);
        Ok(response)
    }
}

fn recorded_posts(name: &str) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let path = format!("{}.posts.json", recording(name));
    let text = std::fs::read_to_string(&path)?;
    let parsed: testkit::Json = serde_json::from_str(&text)?;
    let mut posts = Vec::new();
    for post in parsed.as_array().ok_or("posts file is not an array")? {
        let url = post
            .get("url")
            .and_then(testkit::Json::as_str)
            .ok_or_else(|| format!("{path}: a recorded post carries no url string"))?;
        let body = post
            .get("body")
            .and_then(testkit::Json::as_str)
            .ok_or_else(|| format!("{path}: a recorded post carries no body string"))?;
        posts.push((url.to_owned(), body.to_owned()));
    }
    Ok(posts)
}

fn pretty_json(value: &testkit::Json) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn compare(actual: &[String], recorded: &[(String, String)], label: &str) -> Vec<String> {
    let mut failures = Vec::new();
    if actual.len() != recorded.len() {
        failures.push(format!(
            "{label}: {} post(s), recorded {}",
            actual.len(),
            recorded.len()
        ));
    }
    for (index, (actual_body, (_, recorded_body))) in actual.iter().zip(recorded).enumerate() {
        let Ok(actual_value) = serde_json::from_str::<testkit::Json>(actual_body) else {
            failures.push(format!("{label} post {index}: actual body unreadable"));
            continue;
        };
        let Ok(recorded_value) = serde_json::from_str::<testkit::Json>(recorded_body) else {
            failures.push(format!("{label} post {index}: recorded body unreadable"));
            continue;
        };
        if !json_eq(&recorded_value, &actual_value) {
            failures.push(format!(
                "{label} post {index}: batch body mismatch\n    recorded: {}\n    actual:   {}",
                pretty_json(&recorded_value),
                pretty_json(&actual_value),
            ));
        }
    }
    failures
}

const NOW_TEXT: &str = "2026-10-10T12:00:09.000Z";
async fn writer_replay_case(
    name: &str,
    max_chunk_bytes: usize,
    redaction: Option<capture::redaction::Redaction>,
) -> Result<(), Box<dyn std::error::Error>> {
    let input = std::fs::read_to_string(format!("{}.input.json", recording(name)))?;
    let batch = capture::batch::parse_batch(&input)?;
    let recorded = recorded_posts(name)?;
    let transport = RecordingTransport::new();
    let spool_dir = tempfile::tempdir()?;
    let spool = capture::spool::CaptureSpool::new(spool_dir.path().to_path_buf());
    let writer = capture::writer::CaptureWriter::new(
        transport.clone(),
        spool,
        std::num::NonZeroUsize::new(max_chunk_bytes),
        redaction,
    );
    let now: Timestamp = NOW_TEXT.parse()?;
    let totals = writer.write(&batch, now).await?;
    let posts = transport.posts.lock().clone();
    let mut failures = compare(&posts, &recorded, name);
    if totals.conversation_id != "c-1" {
        failures.push(format!(
            "{name}: conversation_id {}",
            totals.conversation_id
        ));
    }
    let mut total_messages = 0usize;
    for body in &posts {
        let value: testkit::Json = serde_json::from_str(body)
            .map_err(|error| format!("{name}: a posted body is not JSON: {error}"))?;
        let count = value
            .get("messages")
            .and_then(testkit::Json::as_array)
            .map(Vec::len)
            .ok_or_else(|| format!("{name}: a posted body carries no messages array"))?;
        total_messages += count;
    }
    if totals.inserted != total_messages as u64 || totals.skipped != 0 {
        failures.push(format!(
            "{name}: totals inserted {} skipped {}",
            totals.inserted, totals.skipped
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} writer failure(s):\n{}",
            failures.len(),
            failures.join("\n---\n")
        )
        .into())
    }
}

#[tokio::test]
async fn writer_replays_recorded_batches() -> Result<(), Box<dyn std::error::Error>> {
    writer_replay_case("single", 768 * 1024, None).await?;
    writer_replay_case("numbers_and_keys", 768 * 1024, None).await?;
    writer_replay_case("finalize_multi_chunk", 24576, None).await?;
    writer_replay_case("elide_tool_args", 32768, None).await
}

#[tokio::test]
async fn writer_caps_text_at_sixteen_thousand_chars() -> Result<(), Box<dyn std::error::Error>> {
    writer_replay_case("surrogate_cap", 768 * 1024, None).await
}

#[tokio::test]
async fn writer_replays_the_redacted_batch() -> Result<(), Box<dyn std::error::Error>> {
    let env = config::Environment::default();
    let settings = config::RedactionSettings {
        enabled: true,
        builtins: true,
        patterns: vec!["SECRET[0-9]+".to_owned()],
    };
    let redaction =
        capture::redaction::resolve(Some(settings), &env).ok_or("redaction should resolve")?;
    writer_replay_case("redaction", 768 * 1024, Some(redaction)).await
}
