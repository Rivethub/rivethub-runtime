use chrono::{DateTime, Utc};
use kernel::memory::summary_backend::{SummaryIndex, SummaryPassage};
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
    sources: &[(usize, DateTime<Utc>)],
    sections: &[(std::ops::Range<usize>, std::ops::RangeInclusive<usize>)],
) -> Result<SummaryIndex, WorkerError> {
    let chunks = crate::embed::split_embed_text(text, 6000)?;
    let texts = chunks.iter().map(|chunk| chunk.text.clone()).collect();
    let vectors = tokio::time::timeout(embed.timeout, provider.embed(texts))
        .await
        .map_err(|_| WorkerError::EmbedTimeout)?
        .map_err(|error| WorkerError::Embed(error.to_string()))?;
    let width = vectors
        .first()
        .map_or(0, |item| item.vector.len().min(embed.width as usize));
    if width == 0
        || vectors.len() != chunks.len()
        || vectors.iter().any(|item| {
            item.vector.len().min(embed.width as usize) != width
                || item.vector.iter().any(|x| !x.is_finite())
                || item.vector.iter().all(|x| *x == 0.0)
        })
    {
        return Err(WorkerError::Embed("incomplete summary embeddings".into()));
    }
    let vectors: Vec<_> = vectors
        .into_iter()
        .map(|mut item| {
            item.vector.truncate(width);
            item.vector
        })
        .collect();
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

fn time_bounds(
    mut times: impl Iterator<Item = DateTime<Utc>>,
) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let first = times.next()?;
    Some(times.fold((first, first), |(min, max), at| (min.min(at), max.max(at))))
}
