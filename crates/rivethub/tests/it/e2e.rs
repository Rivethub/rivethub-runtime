use jiff::Timestamp;
use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsStr;
use std::time::Duration;

use capture::batch::parse_batch;
use capture::redaction::Redaction;
use capture::spool::CaptureSpool;
use capture::writer::CaptureWriter;
use capture::writer::HttpCaptureTransport;
use config::Config;
use config::Environment;
use config::RedactionSettings;
use serde::Deserialize;
use serde::Serialize;
use tempfile::tempdir;
use testkit::Json;
use tokio_util::sync::CancellationToken;
use wire::capture::CaptureBatchRequest;
use wire::gateway::memory::MemoryBrowseResponse;
use wire::gateway::memory::MemoryHitSource;
use wire::gateway::memory::MemorySearchResponse;
use wire::gateway::memory::MemoryStatsResponse;
use wire::gateway::memory::MemoryToolResponse;
use wire::message::Role;
use wire::message::ToolResult;

type TestResult = Result<(), Box<dyn Error>>;

const WRITER_CASES: [(&str, usize); 6] = [
    ("single", 768 * 1024),
    ("numbers_and_keys", 768 * 1024),
    ("surrogate_cap", 768 * 1024),
    ("finalize_multi_chunk", 24576),
    ("elide_tool_args", 32768),
    ("redaction", 768 * 1024),
];

const NOW: &str = "2026-10-10T12:00:09.000Z";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpectedRow {
    session: String,
    role: Role,
    content: String,
    tool_name: Option<String>,
}

#[derive(Debug, Serialize)]
struct GetFullArgs {
    id: String,
}

#[derive(Debug, Deserialize)]
struct RecordedTotals {
    inserted: u64,
    skipped: u64,
}

fn recording(name: &str) -> std::path::PathBuf {
    testkit::recording("capture/writer").join(name)
}

fn recorded_totals() -> Result<BTreeMap<String, RecordedTotals>, Box<dyn Error>> {
    let text = std::fs::read_to_string(testkit::recording("capture/writer/results.json"))?;
    let parsed: BTreeMap<String, RecordedTotals> = serde_json::from_str(&text)?;
    Ok(parsed)
}

fn recorded_posts(name: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let text = std::fs::read_to_string(recording(name).with_extension("posts.json"))?;
    let parsed: Json = serde_json::from_str(&text)?;
    Ok(parsed
        .as_array()
        .ok_or("posts file is not an array")?
        .iter()
        .map(|post| {
            post.get("body")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned()
        })
        .collect())
}

fn expected_rows(posts: &[String]) -> Result<Vec<ExpectedRow>, Box<dyn Error>> {
    let mut rows = Vec::new();
    for body in posts {
        let batch: Json = serde_json::from_str(body)?;
        let session = batch
            .get("session_key")
            .and_then(Json::as_str)
            .ok_or("recorded post without session_key")?
            .to_owned();
        for message in batch
            .get("messages")
            .and_then(Json::as_array)
            .ok_or("recorded post without messages")?
        {
            rows.push(ExpectedRow {
                session: session.clone(),
                role: message
                    .get("role")
                    .and_then(Json::as_str)
                    .ok_or("recorded message without role")?
                    .parse::<Role>()?,
                content: message
                    .get("content")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                tool_name: message
                    .get("tool_name")
                    .and_then(Json::as_str)
                    .map(str::to_owned),
            });
        }
    }
    Ok(rows)
}

fn redaction_for(name: &str, env: &Environment) -> Option<Redaction> {
    if name != "redaction" {
        return None;
    }
    capture::redaction::resolve(
        Some(RedactionSettings {
            enabled: true,
            builtins: true,
            patterns: vec!["SECRET[0-9]+".to_owned()],
        }),
        env,
    )
}

async fn post_recorded_batches(
    base: &str,
    env: &Environment,
) -> Result<Vec<ExpectedRow>, Box<dyn Error>> {
    let client = reqwest::Client::builder()
        .default_headers(crate::owner::headers(&gateway::state_dir(env)?)?)
        .build()?;
    let now: Timestamp = NOW.parse()?;
    let mut expected = Vec::new();
    for (name, chunk_bytes) in WRITER_CASES {
        let input = std::fs::read_to_string(recording(name).with_extension("input.json"))?;
        let batch: CaptureBatchRequest = parse_batch(&input)?;
        let posts = recorded_posts(name)?;
        let mut rows = expected_rows(&posts)?;
        if name == "surrogate_cap" {
            let original: Json = serde_json::from_str(&input)?;
            let text = original
                .pointer("/messages/0/content")
                .and_then(Json::as_str)
                .ok_or("surrogate_cap input has no content")?;
            for row in &mut rows {
                row.content = text.chars().take(16_000).collect();
            }
        }
        expected.extend(rows);
        let transport = HttpCaptureTransport::new(base.to_owned(), client.clone());
        let spool_dir = tempdir()?;
        let spool = CaptureSpool::new(spool_dir.path().to_path_buf());
        let writer = CaptureWriter::new(
            transport,
            spool,
            std::num::NonZeroUsize::new(chunk_bytes),
            redaction_for(name, env),
        );
        let totals = writer.write(&batch, now).await?;
        let want = recorded_totals()?
            .remove(name)
            .ok_or_else(|| format!("results.json has no entry for {name}"))?;
        assert_eq!(totals.inserted, want.inserted, "{name} inserted");
        assert_eq!(totals.skipped, want.skipped, "{name} skipped");
    }
    Ok(expected)
}

#[tokio::test]
#[expect(clippy::too_many_lines)]
async fn recorded_batches_land_and_read_back_through_the_memory_api() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
    migrate::postgres_pool(&pool).await?;
    let url = db.served_url().await?;
    let home = tempdir()?;
    let env = Environment::from_vars([
        (OsStr::new(config::POSTGRES_URL_VARIABLE), OsStr::new(&url)),
        (OsStr::new("HOME"), home.path().as_os_str()),
        (OsStr::new("RIVETHUB_SHARED_DIR"), home.path().as_os_str()),
    ]);
    let config: Config = serde_yaml_ng::from_str("gateway:\n  host: 127.0.0.1\n  port: 0\n")?;
    let shutdown = CancellationToken::new();
    let activities = kernel::shutdown::Activities::new();
    let memory = rivethub::service::connect(&config, &env, &shutdown, &activities).await?;
    let composition = rivethub::composition::Composition::on_postgres(
        &config,
        &env,
        &memory,
        &shutdown,
        &activities,
    )
    .await?;
    let gateway = std::sync::Arc::clone(composition.gateway());
    let (listener, tls) = rivethub::service::listen(&config, &env).await?;
    assert!(tls.is_none(), "loopback binds resolve without tls");
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(rivethub::service::serve(
        listener,
        tls,
        gateway,
        shutdown.clone(),
    ));
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::builder()
        .default_headers(crate::owner::headers(&gateway::state_dir(&env)?)?)
        .build()?;

    let expected = post_recorded_batches(&base, &env).await?;
    assert_eq!(expected.len(), 11, "recorded posts carry 11 rows");

    let search: MemorySearchResponse = client
        .get(format!("{base}/api/memory/search?q=token&limit=50"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        search.results.len(),
        1,
        "only the recorded redacted row hits"
    );
    let red_row = expected
        .iter()
        .find(|row| row.session == "s-red")
        .ok_or("the recorded redaction row is missing")?;
    let hit = search.results.first().ok_or("search result")?;
    assert_eq!(hit.content, red_row.content);
    assert_eq!(hit.source, MemoryHitSource::Message);
    assert_eq!(hit.role, Some(Role::User));
    assert_eq!(hit.agent.as_deref(), Some("a"));

    let browse: MemoryBrowseResponse = client
        .get(format!("{base}/api/memory/browse?limit=200"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        browse.messages.len(),
        10,
        "browse leaves out the one tool-role row"
    );
    let mut truncated_id = String::new();
    let mut hello_id = String::new();
    for row in &expected {
        if row.role == Role::Tool {
            continue;
        }
        let found = browse
            .messages
            .iter()
            .find(|message| {
                message.session_id.as_deref() == Some(row.session.as_str())
                    && message.content == row.content
            })
            .unwrap_or_else(|| panic!("browse is missing a recorded row: {row:?}"));
        assert_eq!(found.role, row.role, "row role drifted: {row:?}");
        assert_eq!(found.tool_name, row.tool_name, "row tool drifted: {row:?}");
        if row.session == "s-cap" {
            truncated_id.clone_from(&found.id);
        }
        if row.content == "hello" {
            hello_id.clone_from(&found.id);
        }
    }
    assert_ne!(hello_id, "", "the recorded hello row is missing");

    let stats: MemoryStatsResponse = client
        .get(format!("{base}/api/memory/stats"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(stats.conversations, 6);
    assert_eq!(stats.messages, 11);
    assert_eq!(stats.tool_calls, 2);
    assert_eq!(stats.summaries, 0);
    assert_eq!(stats.embedded_messages, 0);
    assert_eq!(stats.failed_embeddings, 0);
    assert_eq!(stats.embed_queue_depth, 10);
    let tools: BTreeMap<&str, u64> = stats
        .top_tools
        .iter()
        .map(|tool| (tool.tool.as_str(), tool.count))
        .collect();
    assert_eq!(tools.get("Bash"), Some(&1));
    assert_eq!(tools.get("Read"), Some(&1));
    assert_eq!(stats.recent_sessions.len(), 6);
    assert_eq!(
        stats
            .recent_sessions
            .iter()
            .map(|session| session.messages)
            .sum::<u64>(),
        11
    );
    let chunked = stats
        .recent_sessions
        .iter()
        .find(|session| session.messages == 6)
        .ok_or("the finalized chunked session is missing from stats")?;
    assert_eq!(
        chunked.title.as_deref(),
        Some("t"),
        "the /api/capture route keeps the batch title"
    );

    let hello_text = get_full_text(&client, &base, &hello_id).await?;
    assert_eq!(
        hello_text,
        "(row was not truncated — stored payload is complete)\n\nhello"
    );
    let truncated_text = get_full_text(&client, &base, &truncated_id).await?;
    assert_eq!(
        truncated_text,
        "Row is truncated but carries no disk pointer (pre-#196 capture, or a non-grok source) — the elided tail is unrecoverable."
    );
    let missing_text =
        get_full_text(&client, &base, "00000000-0000-4000-8000-000000000000").await?;
    assert_eq!(
        missing_text,
        "No message with id 00000000-0000-4000-8000-000000000000."
    );

    shutdown.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(10), server).await??;
    outcome?;
    composition.close().await?;
    Ok(())
}

async fn get_full_text(
    client: &reqwest::Client,
    base: &str,
    id: &str,
) -> Result<String, Box<dyn Error>> {
    let response: MemoryToolResponse = client
        .post(format!("{base}/api/memory/tool/memory_get_full"))
        .json(&GetFullArgs { id: id.to_owned() })
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let ToolResult::Text(text) = response.result else {
        return Err("memory_get_full answered with a non-text result".into());
    };
    Ok(text)
}
