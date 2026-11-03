use std::num::NonZeroUsize;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use serde::Serialize;
use thiserror::Error;
use wire::JsonObject;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::CaptureResult;

use crate::redaction::Redaction;
use crate::spool::CaptureSpool;

const CONTENT_LIMIT: usize = 16_000;
pub const DEFAULT_CHUNK_BYTES: usize = 768 * 1024;
const DEFAULT_REPLAY_MAX: usize = 50;

fn metadata_keep() -> &'static [&'static str] {
    &[
        "event_id",
        "session_jsonl_path",
        "session_jsonl_line",
        "session_sqlite_path",
        "session_sqlite_part_id",
        "truncated",
        "ordinal",
        "native_event_id",
        "source",
        "metadata_elided",
        "full_metadata_bytes",
    ]
}

#[derive(Debug, Error)]
pub enum PostError {
    #[error("capture HTTP {status}")]
    Client { status: u16 },
    #[error("capture HTTP {status}")]
    Server { status: u16 },
    #[error("capture request failed: {message}")]
    Network { message: String },
}

#[derive(Debug, Error)]
pub enum WriteError {
    #[error("chunk exceeds maxChunkBytes after elision")]
    ChunkTooLarge,
    #[error(transparent)]
    Spool(#[from] crate::spool::SpoolError),
    #[error(transparent)]
    Post(#[from] PostError),
    #[error(transparent)]
    Replay(#[from] crate::spool::ReplayError),
    #[error("capture JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Totals {
    pub inserted: u64,
    pub skipped: u64,
    pub conversation_id: String,
}

impl Totals {
    fn add(&mut self, result: &CaptureResult) {
        self.inserted += result.inserted;
        self.skipped += result.skipped;
        if !result.conversation_id.is_empty() {
            self.conversation_id.clone_from(&result.conversation_id);
        }
    }
}

#[async_trait]
pub trait CaptureTransport: Send + Sync {
    async fn post(&self, body: String) -> Result<CaptureResult, PostError>;
}

#[derive(Debug)]
pub struct HttpCaptureTransport {
    endpoint: String,
    client: reqwest::Client,
}

impl HttpCaptureTransport {
    #[must_use]
    pub fn new(endpoint: String, client: reqwest::Client) -> Self {
        Self { endpoint, client }
    }
}

#[async_trait]
impl CaptureTransport for HttpCaptureTransport {
    async fn post(&self, body: String) -> Result<CaptureResult, PostError> {
        let url = format!("{}/api/capture", self.endpoint.trim_end_matches('/'));
        let response = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|error| PostError::Network {
                message: error.to_string(),
            })?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            if (400..500).contains(&status) {
                return Err(PostError::Client { status });
            }
            return Err(PostError::Server { status });
        }
        response
            .json::<CaptureResult>()
            .await
            .map_err(|error| PostError::Network {
                message: error.to_string(),
            })
    }
}

#[derive(Debug)]
pub struct CaptureWriter<T: CaptureTransport> {
    transport: T,
    spool: CaptureSpool,
    max_chunk_bytes: usize,
    redaction: Option<Redaction>,
}

impl<T: CaptureTransport> CaptureWriter<T> {
    #[must_use]
    pub fn new(
        transport: T,
        spool: CaptureSpool,
        max_chunk_bytes: Option<NonZeroUsize>,
        redaction: Option<Redaction>,
    ) -> Self {
        Self {
            transport,
            spool,
            max_chunk_bytes: max_chunk_bytes.map_or(DEFAULT_CHUNK_BYTES, NonZeroUsize::get),
            redaction,
        }
    }

    pub async fn replay(&self, now: DateTime<Utc>) -> Result<(), WriteError> {
        self.spool
            .replay(DEFAULT_REPLAY_MAX, now, |body| self.transport.post(body))
            .await?;
        Ok(())
    }

    pub async fn write(
        &self,
        batch: &CaptureBatchRequest,
        now: DateTime<Utc>,
    ) -> Result<Totals, WriteError> {
        self.replay(now).await?;
        let mut prepared = batch.clone();
        if let Some(redaction) = &self.redaction {
            let spans = redaction.redact_batch(&mut prepared);
            if spans > 0 {
                tracing::info!("redacted {spans} spans");
            }
        }
        cap_batch(&mut prepared)?;
        let chunks = split_chunks(&prepared, self.max_chunk_bytes)?;
        let mut totals = Totals::default();
        let mut rejected = None;
        let mut pending = chunks.iter();
        while let Some(chunk) = pending.next() {
            let body = body_text(chunk)?;
            match self.transport.post(body.clone()).await {
                Ok(result) => totals.add(&result),
                Err(error @ PostError::Client { .. }) => {
                    let moved = self.spool.dead_letter_body(&body, now).await?;
                    tracing::warn!(
                        "capture server rejected a batch for {} ({error}); moved it to {}",
                        chunk.session_key,
                        moved.display()
                    );
                    rejected.get_or_insert(error);
                }
                Err(error) => {
                    self.spool.spool(&body, now).await?;
                    for rest in pending {
                        self.spool.spool(&body_text(rest)?, now).await?;
                    }
                    return Err(error.into());
                }
            }
        }
        match rejected {
            Some(error) => Err(error.into()),
            None => Ok(totals),
        }
    }
}

fn body_text(batch: &CaptureBatchRequest) -> Result<String, WriteError> {
    Ok(serde_json::to_string(batch)?)
}

fn cap_batch(batch: &mut CaptureBatchRequest) -> Result<(), WriteError> {
    for message in &mut batch.messages {
        cap_message(message)?;
    }
    Ok(())
}

fn cap_message(message: &mut CaptureMessage) -> Result<(), serde_json::Error> {
    if let Some(full) = cap_text(&mut message.content) {
        let metadata = message.metadata.get_or_insert_with(JsonObject::default);
        metadata.insert("truncated", true)?;
        metadata.insert("full_content_length", full)?;
    }
    if let Some(full) = message.tool_result.as_mut().and_then(cap_text) {
        let metadata = message.metadata.get_or_insert_with(JsonObject::default);
        metadata.insert("truncated", true)?;
        metadata.insert("full_tool_result_length", full)?;
    }
    Ok(())
}

fn cap_text(text: &mut String) -> Option<usize> {
    let (end, _) = text.char_indices().nth(CONTENT_LIMIT)?;
    let full = text.chars().count();
    text.truncate(end);
    Some(full)
}

#[derive(Serialize)]
struct ElidedBlob {
    #[serde(rename = "_elided")]
    elided: bool,
    bytes: usize,
}

fn elided_blob(bytes: usize) -> Result<JsonObject, serde_json::Error> {
    JsonObject::from_serialize(&ElidedBlob {
        elided: true,
        bytes,
    })
}

fn encoded(object: &JsonObject) -> Result<String, serde_json::Error> {
    serde_json::to_string(object)
}

fn is_full_length_key(key: &str) -> bool {
    key.starts_with("full_") && key.ends_with("_length")
}

fn filter_metadata(
    metadata: &JsonObject,
    original_bytes: usize,
) -> Result<JsonObject, serde_json::Error> {
    let mut kept = metadata.clone();
    kept.retain(|key| metadata_keep().contains(&key) || is_full_length_key(key));
    kept.insert("metadata_elided", true)?;
    kept.insert("full_metadata_bytes", original_bytes)?;
    Ok(kept)
}

fn split_chunks(
    batch: &CaptureBatchRequest,
    max_chunk_bytes: usize,
) -> Result<Vec<CaptureBatchRequest>, WriteError> {
    let mut base = batch.clone();
    base.messages = Vec::new();
    base.finalize = None;
    if let Some(settings) = &base.settings {
        let bytes = encoded(settings)?.len();
        if bytes > max_chunk_bytes {
            base.settings = Some(elided_blob(bytes)?);
        }
    }
    let mut chunks: Vec<CaptureBatchRequest> = Vec::new();
    let mut current = base.clone();
    for message in &batch.messages {
        let mut candidate = current.clone();
        candidate.messages.push(message.clone());
        if body_bytes(&candidate)? <= max_chunk_bytes {
            current = candidate;
            continue;
        }
        if !current.messages.is_empty() {
            chunks.push(std::mem::replace(&mut current, base.clone()));
        }
        current = elided_chunk(&base, message, max_chunk_bytes)?;
    }
    chunks.push(current);
    if let Some(last) = chunks.last_mut() {
        last.finalize = batch.finalize;
    }
    Ok(chunks)
}

fn elided_chunk(
    base: &CaptureBatchRequest,
    original: &CaptureMessage,
    max_chunk_bytes: usize,
) -> Result<CaptureBatchRequest, WriteError> {
    let mut reduced = original.clone();
    if let Some(alone) = chunk_with(base, &reduced, max_chunk_bytes)? {
        return Ok(alone);
    }
    if let Some(arguments) = &original.tool_args {
        let text = encoded(arguments)?;
        reduced.tool_args = Some(elided_blob(text.len())?);
        reduced
            .metadata
            .get_or_insert_with(JsonObject::default)
            .insert("full_tool_args_length", text.chars().count())?;
        if let Some(alone) = chunk_with(base, &reduced, max_chunk_bytes)? {
            return Ok(alone);
        }
    }
    if let Some(existing) = &original.metadata {
        let bytes = encoded(existing)?.len();
        reduced.metadata = Some(filter_metadata(existing, bytes)?);
        if let Some(alone) = chunk_with(base, &reduced, max_chunk_bytes)? {
            return Ok(alone);
        }
    }
    Err(WriteError::ChunkTooLarge)
}

fn chunk_with(
    base: &CaptureBatchRequest,
    message: &CaptureMessage,
    max_chunk_bytes: usize,
) -> Result<Option<CaptureBatchRequest>, WriteError> {
    let mut alone = base.clone();
    alone.messages.push(message.clone());
    Ok((body_bytes(&alone)? <= max_chunk_bytes).then_some(alone))
}

fn body_bytes(batch: &CaptureBatchRequest) -> Result<usize, WriteError> {
    Ok(body_text(batch)?.len())
}
