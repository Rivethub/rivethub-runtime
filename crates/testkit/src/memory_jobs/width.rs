use kernel::memory::EmbedTable;
use kernel::memory::EmbedTarget;
use kernel::memory::EmbeddingStore;
use kernel::memory::MemoryStore;
use kernel::memory::StoredChunk;
use kernel::memory::StoredEmbedding;
use kernel::memory::jobs::ChunkWrite;
use kernel::memory::jobs::MemoryJobError;
use kernel::memory::jobs::MemoryJobStore;

use super::message;
use super::query_vector;
use super::user;
use crate::topics::Outcome;

pub async fn a_chunk_write_rejects_a_vector_wider_than_the_message<S>(store: &S) -> Outcome
where
    S: MemoryStore + MemoryJobStore + EmbeddingStore,
{
    let id = message(
        store,
        user("width", "width body that is long enough to store"),
    )
    .await?;
    store
        .write(StoredEmbedding {
            target: EmbedTarget {
                table: EmbedTable::Messages,
                id: id.to_string(),
            },
            vector: query_vector(),
            content_hash: "test".to_owned(),
            chunks: Vec::new(),
        })
        .await?;
    match store.write_chunks(id, vec![short_span()]).await {
        Ok(ChunkWrite::Written | ChunkWrite::Skipped) => {
            Err("a chunk narrower than the message vector was stored".into())
        }
        Err(MemoryJobError::Query(source)) => {
            let text = source.to_string();
            if text.starts_with("embedding width is ") {
                Ok(())
            } else {
                Err(text.into())
            }
        }
        Err(error @ (MemoryJobError::Arguments(_) | MemoryJobError::MissingTable { .. })) => {
            Err(error.to_string().into())
        }
    }
}

fn short_span() -> StoredChunk {
    StoredChunk {
        index: 0,
        char_start: 0,
        char_end: 1,
        content: "x".to_owned(),
        vector: Some(vec![0.5, 0.0, 0.25]),
    }
}
