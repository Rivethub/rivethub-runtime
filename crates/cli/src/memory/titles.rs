use std::time::Duration;

use futures::StreamExt as _;
use memory_postgres::PostgresMemory;
use memory_postgres::Untitled;
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use workers::Named;
use workers::TitleCall;
use workers::WorkerError;

use crate::Error;
use crate::Report;

const DEFAULT_CONCURRENCY: usize = 2;
const CALL_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::memory) struct TitleModel {
    name: String,
    url: String,
    concurrency: usize,
}

impl TitleModel {
    pub(in crate::memory) fn new(name: String, url: String, concurrency: Option<usize>) -> Self {
        Self {
            name,
            url,
            concurrency: concurrency.unwrap_or(DEFAULT_CONCURRENCY),
        }
    }
}

#[derive(Debug, Default, Serialize)]
struct ModelTitles {
    dry_run: bool,
    candidates: usize,
    titled: usize,
    pairs: Vec<Pair>,
    unfit: Vec<Pair>,
    failed: Vec<Failure>,
}

#[derive(Debug, Serialize)]
struct Pair {
    id: Uuid,
    before: Option<String>,
    after: String,
}

#[derive(Debug, Serialize)]
struct Failure {
    id: Uuid,
    error: String,
}

pub(in crate::memory) async fn backfill(
    url: &str,
    model: &TitleModel,
    dry_run: bool,
    json: bool,
    shutdown: &CancellationToken,
) -> Result<Report, Error> {
    let memory = PostgresMemory::connect(url).await?;
    let untitled = memory.untitled().await?;
    let provider = providers::OpenAiCompatible::build(
        config::ProviderType::Vllm,
        &config::provider::OpenaiProvider {
            base_url: Some(model.url.clone()),
            model: Some(model.name.clone()),
            probe_models: Some(false),
            ..config::provider::OpenaiProvider::default()
        },
    )?
    .with_shutdown(shutdown.child_token());
    let call = TitleCall {
        provider: &provider,
        model: &model.name,
        cancel: shutdown,
    };
    let mut outcome = ModelTitles {
        dry_run,
        candidates: untitled.len(),
        ..ModelTitles::default()
    };
    let call = &call;
    let mut answers = futures::stream::iter(untitled)
        .map(|item| async move {
            let named = name(call, &item).await;
            (item, named)
        })
        .buffer_unordered(model.concurrency);
    while let Some((item, named)) = answers.next().await {
        let (id, before) = (item.head.id, item.head.title.clone());
        match named {
            Ok(Named::Title(after)) => {
                if dry_run || memory.apply_opening_title(&item, Some(&after)).await? {
                    outcome.titled += 1;
                    outcome.pairs.push(Pair { id, before, after });
                }
            }
            Ok(Named::Unfit(after)) => outcome.unfit.push(Pair { id, before, after }),
            Err(WorkerError::Cancelled) => return Err(Error::Cancelled),
            Err(error) => outcome.failed.push(Failure {
                id,
                error: error.to_string(),
            }),
        }
    }
    outcome.pairs.sort_by_key(|pair| pair.id);
    if json {
        return Ok(Report::success(format!(
            "{}\n",
            serde_json::to_string(&outcome)?
        )));
    }
    Ok(Report::success(render(&outcome)))
}

async fn name(call: &TitleCall<'_>, item: &Untitled) -> Result<Named, WorkerError> {
    tokio::time::timeout(CALL_TIMEOUT, call.name(&[&item.content]))
        .await
        .map_err(|_| WorkerError::ModelTimeout {
            seconds: CALL_TIMEOUT.as_secs(),
        })?
}

fn render(outcome: &ModelTitles) -> String {
    let verb = if outcome.dry_run {
        "would title"
    } else {
        "titled"
    };
    let header = format!(
        "{verb} {} of {} conversation(s); {} unfit answer(s), {} failed call(s)",
        outcome.titled,
        outcome.candidates,
        outcome.unfit.len(),
        outcome.failed.len()
    );
    let named = outcome.pairs.iter().map(|pair| {
        format!(
            "{} {} -> {:?}",
            pair.id,
            shown(pair.before.as_deref()),
            pair.after
        )
    });
    let kept = outcome.unfit.iter().map(|pair| {
        format!(
            "{} kept {} (unfit answer {:?})",
            pair.id,
            shown(pair.before.as_deref()),
            pair.after
        )
    });
    let failed = outcome
        .failed
        .iter()
        .map(|failure| format!("{} failed: {}", failure.id, failure.error));
    let mut lines: Vec<String> = std::iter::once(header)
        .chain(named)
        .chain(kept)
        .chain(failed)
        .collect();
    lines.push(String::new());
    lines.join("\n")
}

fn shown(title: Option<&str>) -> String {
    match title {
        Some(title) => format!("{title:?}"),
        None => "(untitled)".to_owned(),
    }
}
