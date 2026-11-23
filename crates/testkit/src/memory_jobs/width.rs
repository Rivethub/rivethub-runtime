use kernel::memory::EmbedTable;
use kernel::memory::EmbedTarget;
use kernel::memory::EmbeddingStore;
use kernel::memory::EmbeddingStoreError;
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

pub async fn a_chunk_written_first_fixes_the_store_width<S>(store: &S) -> Outcome
where
    S: MemoryStore + MemoryJobStore + EmbeddingStore,
{
    let first = message(store, user("width-a", "first note")).await?;
    let second = message(store, user("width-b", "second note")).await?;
    store.write_chunks(first, vec![narrow()]).await?;
    let wide = query_vector();
    let width = wide.len();
    match store
        .write(StoredEmbedding {
            target: EmbedTarget {
                table: EmbedTable::Messages,
                id: second.to_string(),
            },
            vector: wide,
            content_hash: "test".to_owned(),
            chunks: Vec::new(),
        })
        .await
    {
        Err(EmbeddingStoreError::Width { got, expected }) => {
            assert_eq!((got, expected), (width, 3));
            Ok(())
        }
        Ok(()) => Err("a wider vector was stored after a narrow chunk".into()),
        Err(
            error @ (EmbeddingStoreError::SummaryReference(_)
            | EmbeddingStoreError::Query { .. }
            | EmbeddingStoreError::Missing { .. }
            | EmbeddingStoreError::TargetId { .. }
            | EmbeddingStoreError::Model { .. }
            | EmbeddingStoreError::Chunks { .. }
            | EmbeddingStoreError::ChunkIndex
            | EmbeddingStoreError::FailureCount
            | EmbeddingStoreError::Closed),
        ) => Err(error.to_string().into()),
    }
}

fn narrow() -> StoredChunk {
    StoredChunk {
        index: 0,
        char_start: 0,
        char_end: 4,
        content: "note".to_owned(),
        vector: Some(vec![1.0, 0.0, 0.0]),
    }
}

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
