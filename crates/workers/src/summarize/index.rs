use jiff::Timestamp;
use kernel::memory::summary_backend::{SUMMARY_EMBEDDING_WIDTH, SummaryIndex, SummaryPassage};
use kernel::model::ModelProvider;
use uuid::Uuid;

use super::live::integer;
use crate::{EmbedSettings, WorkerError};

pub(crate) async fn repair(
    store: &dyn kernel::memory::SummaryStore,
    provider: &dyn ModelProvider,
    embed: EmbedSettings,
    id: Uuid,
) -> Result<bool, WorkerError> {
    let Some(input) = store.repair_input(id).await? else {
        return Ok(false);
    };
    let sources = input
        .sources
        .iter()
        .map(|&(ordinal, at)| Ok((unsigned(ordinal)?, at)))
        .collect::<Result<Vec<_>, WorkerError>>()?;
    let sections = input
        .sections
        .iter()
        .map(|section| {
            Ok((
                unsigned(section.char_start)?..unsigned(section.char_end)?,
                unsigned(section.first)?..=unsigned(section.last)?,
            ))
        })
        .collect::<Result<Vec<_>, WorkerError>>()?;
    let index = prepare_index(provider, embed, &input.content, &sources, &sections).await?;
    store.repair_index(input, index).await?;
    Ok(true)
}

fn unsigned(value: i32) -> Result<usize, WorkerError> {
    usize::try_from(value).map_err(|_| WorkerError::Count)
}

pub(super) async fn prepare_index(
    provider: &dyn ModelProvider,
    embed: EmbedSettings,
    text: &str,
    sources: &[(usize, Timestamp)],
    sections: &[(std::ops::Range<usize>, std::ops::RangeInclusive<usize>)],
) -> Result<SummaryIndex, WorkerError> {
    let width = SUMMARY_EMBEDDING_WIDTH;
    if !matches!(usize::try_from(embed.width), Ok(got) if got == width) {
        return Err(WorkerError::EmbedWidth {
            got: embed.width,
            expected: width,
        });
    }
    let chunks = crate::embed::split_embed_text(text, 6000)?;
    let texts = chunks.iter().map(|chunk| chunk.text.clone()).collect();
    let vectors = tokio::time::timeout(embed.timeout, provider.embed(texts))
        .await
        .map_err(|_| WorkerError::EmbedTimeout)?
        .map_err(|error| WorkerError::Embed(error.to_string()))?;
    if vectors.len() != chunks.len() {
        return Err(WorkerError::Embed("incomplete summary embeddings".into()));
    }
    let vectors: Vec<_> = vectors.into_iter().map(|item| item.vector).collect();
    for vector in &vectors {
        if vector.len() != width {
            return Err(WorkerError::Embed(format!(
                "summary embedding has {} dimensions; expected {width}",
                vector.len()
            )));
        }
        if vector.iter().any(|x| !x.is_finite()) || vector.iter().all(|x| *x == 0.0) {
            return Err(WorkerError::NullEmbedding);
        }
    }
    let pooled = crate::embed::mean_pool(&vectors, width)?.ok_or(WorkerError::NullEmbedding)?;
    if pooled.iter().any(|x| !x.is_finite()) || pooled.iter().all(|x| *x == 0.0) {
        return Err(WorkerError::NullEmbedding);
    }
    let passages = chunks
        .into_iter()
        .zip(vectors)
        .map(|(chunk, vector)| {
            let (start, end) = (chunk.char_start as usize, chunk.char_end as usize);
            let times = sources
                .iter()
                .filter(|(ordinal, _)| {
                    sections.iter().any(|(chars, bounds)| {
                        chars.start < end && start < chars.end && bounds.contains(ordinal)
                    })
                })
                .map(|(_, at)| *at);
            let bounds =
                time_bounds(times).or_else(|| time_bounds(sources.iter().map(|(_, at)| *at)));
            let (earliest_at, latest_at) = bounds.ok_or(WorkerError::Count)?;
            Ok(SummaryPassage {
                content: chunk.text,
                chars: integer(start)?..integer(end)?,
                earliest_at,
                latest_at,
                vector,
            })
        })
        .collect::<Result<_, WorkerError>>()?;
    Ok(SummaryIndex { passages, pooled })
}

fn time_bounds(mut times: impl Iterator<Item = Timestamp>) -> Option<(Timestamp, Timestamp)> {
    let first = times.next()?;
    Some(times.fold((first, first), |(min, max), at| (min.min(at), max.max(at))))
}

#[cfg(test)]
mod probes {
    use super::*;
    use async_trait::async_trait;
    use kernel::model::{
        Embedding, LlmResponse, ModelProvider, ModelProviderError, ModelRequest, ModelStreamItem,
    };
    use tokio::sync::mpsc::Receiver;

    struct Fixed(Vec<Vec<f32>>);

    #[async_trait]
    impl ModelProvider for Fixed {
        async fn chat(&self, _: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
            unreachable!("chat");
        }

        async fn stream(
            &self,
            _: ModelRequest,
        ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
            unreachable!("stream");
        }

        async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
            assert_eq!(texts.len(), self.0.len());
            Ok(self
                .0
                .iter()
                .cloned()
                .map(|vector| Embedding { vector })
                .collect())
        }

        async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn summary_embedding_requires_the_full_column_width() {
        for got in [2, 1024, 1536] {
            let result = prepare_index(
                &Fixed(vec![vec![0.25; got]]),
                EmbedSettings::standard(),
                "saffron",
                &[(1, Timestamp::UNIX_EPOCH)],
                &[(0..7, 1..=1)],
            )
            .await;
            if got == 1024 {
                assert_eq!(
                    result
                        .expect("full width")
                        .passages
                        .first()
                        .expect("passage")
                        .vector
                        .len(),
                    got
                );
            } else {
                let error = result.expect_err("wrong embedding width");
                assert!(matches!(error, WorkerError::Embed(_)));
                assert!(
                    error
                        .to_string()
                        .contains(&format!("{got} dimensions; expected 1024"))
                );
            }
        }
    }
}
