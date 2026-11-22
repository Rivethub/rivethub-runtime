use std::path::Path;
use std::time::Duration;

use rusqlite::OptionalExtension as _;
use wire::gateway::memory::{MemoryHitSource, MemorySearchResponse};

use super::{REPLY, TestResult, provider_at};

const EMBED_MODEL: &str = "m";

pub(super) fn config(home: &Path, database: &Path, endpoint: &str, port: u16) -> String {
    provider_at(
        home,
        endpoint,
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    workers: true\n    embed_endpoint: {endpoint}\n    embed_model: {EMBED_MODEL}\ngateway:\n  host: 127.0.0.1\n  port: {port}\n",
            database.display()
        ),
    )
}

pub(super) fn index_key(endpoint: &str) -> [u8; 32] {
    kernel::memory::summary_backend::index_key(
        endpoint,
        None,
        EMBED_MODEL,
        workers::EmbedSettings::standard().width,
    )
}

pub(super) struct IndexedSession {
    pub id: String,
    pub content: String,
}

pub(super) fn indexed_session(
    database: &Path,
    conversation: &str,
    key: [u8; 32],
) -> TestResult<Option<IndexedSession>> {
    let conn = rusqlite::Connection::open(database)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn
        .query_row(
            "SELECT s.id, s.content FROM ros_summaries s
               JOIN ros_summary_records_indexed complete ON complete.id = s.id
              WHERE s.conversation_id = ?1 AND s.kind = 'session'
                AND NOT s.index_needs_repair AND s.index_key = ?2
                AND EXISTS (
                    SELECT 1 FROM ros_summary_sources src
                    JOIN ros_messages m ON m.id = src.message_id
                    WHERE src.summary_id = s.id AND m.conversation_id = s.conversation_id
                )",
            rusqlite::params![conversation, key.as_slice()],
            |row| {
                Ok(IndexedSession {
                    id: row.get(0)?,
                    content: row.get(1)?,
                })
            },
        )
        .optional()?)
}

pub(super) async fn assert_vector_hit(
    home: &Path,
    port: u16,
    summary: &IndexedSession,
) -> TestResult {
    let response = reqwest::Client::builder()
        .no_proxy()
        .default_headers(crate::owner::home_headers(home)?)
        .build()?
        .get(format!(
            "http://127.0.0.1:{port}/api/memory/search?q=ledger%20is%20finished&mode=vector&scope=summaries"
        ))
        .send()
        .await?
        .error_for_status()?
        .json::<MemorySearchResponse>()
        .await?;
    assert_eq!(response.degraded, None, "{response:?}");
    assert_eq!(response.fallback, None, "{response:?}");
    assert!(
        response.results.iter().any(|hit| {
            hit.id == summary.id
                && hit.source == MemoryHitSource::Summary
                && hit.passage.is_some()
                && hit.content == REPLY
        }),
        "{response:?}"
    );
    Ok(())
}
