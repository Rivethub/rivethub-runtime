use std::time::Duration;

use kernel::memory::EmbedTable;
use kernel::memory::EmbedTarget;
use kernel::memory::EmbeddingStore;
use kernel::memory::EmbeddingStoreError;
use kernel::memory::StoredChunk;
use kernel::memory::StoredEmbedding;
use kernel::model::Embedding;
use kernel::model::ModelProvider;
use kernel::model::ModelProviderError;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;

use super::classify::classify_unembeddable;
use super::pool::mean_pool;
use super::pool::shared_length;
use super::settings::EmbedSettings;
use super::split::TextChunk;
use super::split::split_embed_text;
use super::split::stored_chunk;
use super::target::ParsedTarget;
use super::target::read_target;
use super::text::compose;
use super::text::content_hash;
use crate::error::WorkerError;

pub async fn handle_embed(
    targets: &dyn EmbeddingStore,
    provider: &dyn ModelProvider,
    settings: EmbedSettings,
    payload: &JsonObject,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    cancelled(cancel)?;
    let target = match read_target(payload)? {
        ParsedTarget::Ready(target) => target,
        ParsedTarget::UnknownTable => {
            tracing::info!("[embed-target] invalid target_table");
            return Ok(());
        }
        ParsedTarget::EmptyId => return Ok(()),
    };
    if chunk_table_disabled(target.table, settings) {
        tracing::info!("[embed-target] skipped — chunks disabled");
        return Ok(());
    }
    let Some(loaded) = targets.load(&target).await? else {
        tracing::info!("[embed-target] not found or empty — dropping");
        return Ok(());
    };
    let text = compose(
        target.table,
        &loaded,
        usize::try_from(settings.tool_result_cap).map_err(|_| WorkerError::TextTooLong)?,
    );
    if text.is_empty() {
        tracing::info!("[embed-target] empty after compose — dropping");
        return Ok(());
    }
    if let Some(reason) = classify_unembeddable(&text) {
        ignore_expired(targets.mark_unembeddable(&target, reason).await)?;
        return Ok(());
    }
    if settings.width == 0 {
        return Err(WorkerError::Width);
    }
    let pieces = split_embed_text(
        &text,
        usize::try_from(settings.chars_per_chunk).map_err(|_| WorkerError::TextTooLong)?,
    )?;
    let embeddings = embed_pieces(provider, &pieces, settings.timeout, cancel).await?;
    if embeddings.len() != pieces.len() {
        return Err(WorkerError::Embed(
            "the provider returned a different number of embeddings than chunks".to_owned(),
        ));
    }
    let width = usize::try_from(settings.width).map_err(|_| WorkerError::Width)?;
    let vectors: Vec<Vec<f32>> = embeddings.into_iter().map(|row| row.vector).collect();
    let Some(vector) = mean_pool(&vectors, width)? else {
        return match null_vector(targets, &target, settings).await {
            Err(WorkerError::Target(EmbeddingStoreError::SummaryReference(_))) => Ok(()),
            result => result,
        };
    };
    let stored = stored_embedding(&target, &text, vector, &pieces, &vectors, settings)?;
    ignore_expired(targets.write(stored).await)?;
    Ok(())
}

fn chunk_table_disabled(table: EmbedTable, settings: EmbedSettings) -> bool {
    table == EmbedTable::MessageChunks && !settings.chunks_enabled
}

async fn embed_pieces(
    provider: &(dyn ModelProvider + Sync),
    pieces: &[TextChunk],
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Vec<Embedding>, WorkerError> {
    let texts: Vec<String> = pieces.iter().map(|piece| piece.text.clone()).collect();
    cancelled(cancel)?;
    let embed = provider.embed(texts);
    tokio::pin!(embed);
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(WorkerError::Cancelled),
        result = tokio::time::timeout(timeout, &mut embed) => match result {
            Ok(Ok(rows)) => Ok(rows),
            Ok(Err(error)) => Err(embed_error(&error)),
            Err(_elapsed) => Err(WorkerError::EmbedTimeout),
        },
    }
}

async fn null_vector(
    targets: &dyn EmbeddingStore,
    target: &EmbedTarget,
    settings: EmbedSettings,
) -> Result<(), WorkerError> {
    let failures = targets
        .record_null(target, crate::error::NULL_EMBED_ERROR)
        .await?;
    if failures >= settings.max_failures {
        targets
            .mark_permanent(target, crate::error::PERMANENT_NULL_EMBED_ERROR)
            .await?;
        tracing::info!("[embed-target] poisoned after {failures} null embeddings — marking failed");
        return Ok(());
    }
    Err(WorkerError::NullEmbedding)
}

fn stored_embedding(
    target: &EmbedTarget,
    text: &str,
    vector: Vec<f32>,
    pieces: &[TextChunk],
    vectors: &[Vec<f32>],
    settings: EmbedSettings,
) -> Result<StoredEmbedding, WorkerError> {
    let chunks = chunk_rows(target.table, pieces, vectors, settings)?;
    Ok(StoredEmbedding {
        target: target.clone(),
        vector,
        content_hash: content_hash(text),
        chunks,
    })
}

fn chunk_rows(
    table: EmbedTable,
    pieces: &[TextChunk],
    vectors: &[Vec<f32>],
    settings: EmbedSettings,
) -> Result<Vec<StoredChunk>, WorkerError> {
    if pieces.len() <= 1 || table != EmbedTable::Messages || !settings.chunks_enabled {
        return Ok(Vec::new());
    }
    let Some(pooled_len) = shared_length(vectors) else {
        return Ok(Vec::new());
    };
    let mut chunks = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        let Some(raw) = vectors.get(index) else {
            return Err(WorkerError::Embed(
                "the provider returned fewer embeddings than chunks".to_owned(),
            ));
        };
        if raw.len() == pooled_len {
            chunks.push(stored_chunk(index, piece, Some(raw.clone()))?);
        }
    }
    Ok(chunks)
}

fn embed_error(error: &ModelProviderError) -> WorkerError {
    match crate::outage::provider_outage(error) {
        Some(outage) => outage,
        None => WorkerError::Embed(error.to_string()),
    }
}

fn cancelled(cancel: &CancellationToken) -> Result<(), WorkerError> {
    if cancel.is_cancelled() {
        Err(WorkerError::Cancelled)
    } else {
        Ok(())
    }
}

fn ignore_expired(result: Result<(), EmbeddingStoreError>) -> Result<(), EmbeddingStoreError> {
    match result {
        Err(EmbeddingStoreError::SummaryReference(_)) => Ok(()),
        result => result,
    }
}
