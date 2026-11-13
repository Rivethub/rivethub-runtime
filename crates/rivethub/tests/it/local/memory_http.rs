use std::io::Read as _;
use std::path::Path;

use wire::capture::{CaptureBatchRequest, CaptureMessage};
use wire::gateway::memory::{MemorySummaryDagResponse, SummaryKind};
use wire::message::Role;

use super::{TestResult, start, stop};

const SESSION: &str = "memory-http";
const BRANCH: &str = "00000000-0000-4000-8000-0000000000b1";
const LEAF: &str = "00000000-0000-4000-8000-0000000000a1";

fn batch() -> CaptureBatchRequest {
    let message = |event_id: &str, role: Role, content: &str| CaptureMessage {
        event_id: event_id.to_owned(),
        role,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    };
    CaptureBatchRequest {
        session_key: SESSION.to_owned(),
        agent: "claude-code".to_owned(),
        channel: Some("claude-code".to_owned()),
        title: Some("Export".to_owned()),
        settings: None,
        task_id: None,
        finalize: Some(true),
        created_at: None,
        updated_at: None,
        messages: vec![
            message("evt-1", Role::User, "AcmeKit ships on Fridays"),
            message("evt-2", Role::Assistant, "Noted for AcmeKit"),
        ],
    }
}

fn seed_summaries(database: &Path) -> TestResult {
    let conn = rusqlite::Connection::open(database)?;
    let conversation: String = conn.query_row(
        "SELECT id FROM ros_conversations WHERE session_key = ?1",
        [SESSION],
        |row| row.get(0),
    )?;
    let at = "2026-10-10T12:00:00.000Z";
    for (id, kind, content) in [
        (BRANCH, "branch", "AcmeKit release branch summary"),
        (LEAF, "leaf", "AcmeKit release leaf summary"),
    ] {
        conn.execute(
            "INSERT INTO ros_summaries (id, conversation_id, content, kind, message_count, earliest_at, latest_at, created_at) \
             VALUES (?1, ?2, ?3, ?4, 2, ?5, ?5, ?5)",
            (id, &conversation, content, kind, at),
        )?;
    }
    conn.execute(
        "UPDATE ros_summaries SET parent_id = ?1 WHERE id = ?2",
        (BRANCH, LEAF),
    )?;
    conn.execute(
        "INSERT INTO ros_summary_sources (summary_id, message_id, ordinal) \
         SELECT ?1, id, row_number() OVER (ORDER BY created_at, id) - 1 FROM ros_messages WHERE conversation_id = ?2",
        (LEAF, &conversation),
    )?;
    Ok(())
}

fn gunzip(bytes: &[u8]) -> TestResult<String> {
    let mut text = String::new();
    flate2::read::GzDecoder::new(bytes).read_to_string(&mut text)?;
    Ok(text)
}

async fn exchange(
    port: u16,
    database: &Path,
) -> TestResult<(MemorySummaryDagResponse, String, String)> {
    let state = database.parent().ok_or("database has no directory")?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .default_headers(crate::owner::headers(state)?)
        .build()?;
    let base = format!("http://127.0.0.1:{port}");
    client
        .post(format!("{base}/api/capture"))
        .json(&batch())
        .send()
        .await?
        .error_for_status()?;
    seed_summaries(database)?;
    let dag = client
        .get(format!("{base}/api/memory/summaries/{SESSION}"))
        .send()
        .await?
        .error_for_status()?
        .json::<MemorySummaryDagResponse>()
        .await?;
    let export = client
        .get(format!("{base}/api/memory/export"))
        .send()
        .await?
        .error_for_status()?;
    let media = export
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .ok_or("export content type")?
        .to_str()?
        .to_owned();
    let dump = gunzip(&export.bytes().await?)?;
    Ok((dag, media, dump))
}

#[tokio::test]
async fn local_serves_the_summary_dag_and_a_gzip_export_over_http() -> TestResult {
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let port_text = port.to_string();
    let started = start(
        home.path(),
        &["local", "--no-lan", "--port", &port_text],
        &["gateway listening on "],
    )
    .await?;
    let database = home.path().join(".rivethub").join("memory.sqlite");
    let outcome = exchange(port.number(), &database).await;
    stop(started).await?;
    let (dag, media, dump) = outcome?;
    assert_eq!(dag.session_id, SESSION);
    let shape: Vec<_> = dag
        .summaries
        .iter()
        .map(|node| {
            (
                node.id.as_str(),
                node.parent_id.as_deref(),
                node.level,
                node.kind,
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            (BRANCH, None, 1, SummaryKind::Branch),
            (LEAF, Some(BRANCH), 0, SummaryKind::Leaf),
        ]
    );
    assert_eq!(
        dag.summaries.get(1).ok_or("leaf")?.source_message_ids.len(),
        2
    );
    assert_eq!(
        (
            dag.messages.total,
            dag.messages.user,
            dag.messages.assistant
        ),
        (2, 1, 1)
    );
    assert_eq!(media, "application/gzip");
    assert!(dump.contains("-- source local"), "{dump}");
    assert!(dump.contains("AcmeKit ships on Fridays"), "{dump}");
    assert!(dump.contains("AcmeKit release leaf summary"), "{dump}");
    Ok(())
}
