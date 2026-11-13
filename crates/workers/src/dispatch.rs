use crate::embed::{EmbedSettings, handle_embed};
use crate::entity_builder::{
    EntityBuilderDeps, EntityBuilderEnv, handle_enqueue_stale_entities, handle_entity_backfill,
    handle_extract_entities,
};
use crate::error::WorkerError;
use crate::filing::{FilingEnv, SessionTopics, file_target, handle_file};
use crate::role::ProcessRole;
use crate::summarize::{
    SummarizeEnv, SummarySettings, handle_enqueue_idle, handle_enqueue_stale, handle_reap,
    handle_summarize,
};
use crate::wiki_builder::{
    WikiBuilderDeps, WikiBuilderEnv, handle_consolidate_wiki, handle_extract_wiki,
    handle_wiki_backfill,
};
use chrono::Utc;
use kernel::memory::{EmbeddingStore, SummaryStore};
use kernel::model::ModelProvider;
use kernel::queue::{ClaimedJob, FixedJob, JobName, JobQueue};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) struct JobEnv {
    pub(crate) settings: crate::MemoryJobSettings,
    pub(crate) wiki_enabled: bool,
    pub(crate) memory_jobs: Option<Arc<dyn kernel::memory::jobs::MemoryJobStore>>,
    pub(crate) unembedded: Option<Arc<dyn kernel::memory::jobs::UnembeddedStore>>,
    pub(crate) wiki: Option<WikiBuilderDeps>,
    pub(crate) entity: Option<EntityBuilderDeps>,
    pub(crate) targets: Arc<dyn EmbeddingStore>,
    pub(crate) summaries: Arc<dyn SummaryStore>,
    pub(crate) summarizer: Arc<dyn ModelProvider>,
    pub(crate) categorizer: crate::RoleModel,
    pub(crate) embedder: Arc<dyn ModelProvider>,
    pub(crate) embed: EmbedSettings,
    pub(crate) summary: SummarySettings,
    pub(crate) topics: Option<SessionTopics>,
    pub(crate) topic_pages: Option<crate::topic_page::TopicPages>,
    pub(crate) tidy: Option<Arc<dyn kernel::tidy::TidyStore>>,
    pub(crate) deletions: Option<Arc<dyn kernel::memory::SessionDeletion>>,
    pub(crate) digests: Option<crate::digest::DailyDigests>,
    pub(crate) scopes: Option<crate::OwnerScopes>,
}

pub(crate) async fn dispatch<Q: JobQueue>(
    queue: &Q,
    env: &JobEnv,
    job: &ClaimedJob,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let summarize = SummarizeEnv {
        store: env.summaries.as_ref(),
        provider: env.summarizer.as_ref(),
        queue,
        settings: &env.summary,
        now: Utc::now(),
        cancel,
    };
    match &job.name {
        JobName::Fixed(FixedJob::ExtractWiki | FixedJob::EnqueueWikiBackfill) => {
            dispatch_wiki(queue, env, job, cancel).await
        }
        JobName::Fixed(FixedJob::EmbedTarget) => {
            handle_embed(
                env.targets.as_ref(),
                env.embedder.as_ref(),
                env.embed,
                &job.payload,
                cancel,
            )
            .await
        }
        JobName::Fixed(FixedJob::EnqueueUnembedded) => {
            dispatch_unembedded(queue, env, cancel).await
        }
        JobName::Fixed(FixedJob::EnqueueUnchunked) => {
            dispatch_sweep(queue, env, Sweep::Unchunked, cancel).await
        }
        JobName::Fixed(FixedJob::EnqueueStaleWiki) => {
            dispatch_sweep(queue, env, Sweep::StaleWiki, cancel).await
        }
        JobName::Fixed(FixedJob::SynthesizeToolCall) => {
            let store = env
                .memory_jobs
                .as_ref()
                .ok_or(WorkerError::MissingMemoryJobs)?;
            crate::tool_synthesis::handle_synthesize_tool_call(
                store.as_ref(),
                env.summarizer.as_ref(),
                &env.summary.model,
                &job.payload,
                cancel,
            )
            .await
        }
        JobName::Fixed(FixedJob::SummarizeConversation) => {
            handle_summarize(&summarize, &job.payload).await.map(|_| ())
        }
        JobName::Fixed(FixedJob::EnqueueIdle) => handle_enqueue_idle(&summarize).await.map(|_| ()),
        JobName::Fixed(FixedJob::EnqueueStaleSummaries) => {
            handle_enqueue_stale(&summarize).await.map(|_| ())
        }
        JobName::Fixed(FixedJob::ReapDeadJobs) => handle_reap(&summarize).await.map(|_| ()),
        JobName::Fixed(FixedJob::FileConversation) => dispatch_file(env, job, cancel).await,
        JobName::Fixed(FixedJob::ProposeTidy) => dispatch_tidy(env, cancel).await,
        JobName::Fixed(FixedJob::ConsolidateWiki) => {
            dispatch_consolidate(queue, env, job, cancel).await
        }
        JobName::Fixed(FixedJob::RecompileWiki) => {
            dispatch_recompile(queue, env, job, cancel).await
        }
        JobName::Fixed(FixedJob::SyncTopicPage) => dispatch_topic_page(env, job, cancel).await,
        JobName::Fixed(FixedJob::PurgeDeletedSessions) => dispatch_purge(env, cancel).await,
        JobName::Fixed(FixedJob::DigestDay) => dispatch_digest(env, job, cancel).await,
        JobName::Fixed(
            FixedJob::ExtractEntities
            | FixedJob::EnqueueEntityBackfill
            | FixedJob::EnqueueStaleEntities,
        ) => dispatch_entities(queue, env, job, cancel).await,
        JobName::Heartbeat { .. } => Err(WorkerError::Unhandled {
            name: job.name.to_string(),
        }),
    }
}

async fn dispatch_file(
    env: &JobEnv,
    job: &ClaimedJob,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let conversation = file_target(&job.payload)?;
    let topics = env
        .topics
        .as_ref()
        .ok_or(WorkerError::MissingDependencies {
            role: ProcessRole::Categorizer,
        })?;
    let filing = FilingEnv {
        topics,
        provider: env.categorizer.provider.as_ref(),
        model: &env.categorizer.model,
        now: Utc::now(),
        cancel,
    };
    let scope = crate::scopes::conversation_scope(env.scopes.as_ref(), conversation).await?;
    crate::scopes::within(scope, handle_file(&filing, conversation))
        .await
        .map(|_| ())
}

async fn dispatch_digest(
    env: &JobEnv,
    job: &ClaimedJob,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let deps = env
        .digests
        .as_ref()
        .ok_or(WorkerError::MissingDependencies {
            role: ProcessRole::Summarizer,
        })?;
    let request = crate::digest::digest_request(&job.payload)?;
    let owner = request.owner()?;
    let digest = crate::digest::DigestEnv {
        deps,
        provider: env.summarizer.as_ref(),
        model: &env.summary.model,
        cancel,
    };
    let scope = crate::scopes::owner_scope(env.scopes.as_ref(), &owner).await?;
    crate::scopes::within(scope, crate::digest::handle_digest_day(&digest, &request)).await
}

async fn dispatch_purge(env: &JobEnv, cancel: &CancellationToken) -> Result<(), WorkerError> {
    let deletions = env
        .deletions
        .as_ref()
        .ok_or(WorkerError::MissingDependencies {
            role: ProcessRole::Summarizer,
        })?;
    crate::deleted_sessions::handle_purge(
        deletions.as_ref(),
        env.settings.trash_days,
        Utc::now(),
        cancel,
    )
    .await
    .map(|_| ())
}

async fn dispatch_tidy(env: &JobEnv, cancel: &CancellationToken) -> Result<(), WorkerError> {
    let (Some(topics), Some(tidy)) = (&env.topics, &env.tidy) else {
        return Err(WorkerError::MissingDependencies {
            role: ProcessRole::Categorizer,
        });
    };
    let run = crate::tidy::TidyEnv {
        topics,
        tidy: tidy.as_ref(),
        provider: env.categorizer.provider.as_ref(),
        model: &env.categorizer.model,
        now: Utc::now(),
        cancel,
    };
    let scope = crate::scopes::primary_scope(env.scopes.as_ref()).await?;
    crate::scopes::within(scope, crate::tidy::handle_tidy(&run))
        .await
        .map(|_| ())
}

async fn dispatch_topic_page(
    env: &JobEnv,
    job: &ClaimedJob,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let deps = env
        .topic_pages
        .as_ref()
        .ok_or(WorkerError::MissingDependencies {
            role: ProcessRole::Summarizer,
        })?;
    let scope = crate::scopes::primary_scope(env.scopes.as_ref()).await?;
    crate::scopes::within(
        scope,
        crate::topic_page::handle_sync_topic_page(deps, &job.payload, cancel),
    )
    .await
}

async fn dispatch_wiki<Q: JobQueue>(
    queue: &Q,
    env: &JobEnv,
    job: &ClaimedJob,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    if !env.wiki_enabled {
        return Ok(());
    }
    let wiki = env.wiki.as_ref().ok_or(WorkerError::MissingDependencies {
        role: ProcessRole::WikiBuilder,
    })?;
    let topics = match (&env.topics, &env.topic_pages) {
        (Some(sessions), Some(pages)) => Some(kernel::wiki::route::TopicPorts {
            topics: sessions.store.as_ref(),
            pages: pages.pages.as_ref(),
            filing: sessions.filing.as_ref(),
            lineage: sessions.lineage.as_ref(),
        }),
        (None, _) | (_, None) => None,
    };
    let wiki_env = WikiBuilderEnv {
        store: wiki.store.as_ref(),
        provider: wiki.provider.as_ref(),
        queue,
        model: &wiki.model,
        now: Utc::now(),
        cancel,
        topics,
    };
    if job.name == JobName::Fixed(FixedJob::ExtractWiki) {
        handle_extract_wiki(&wiki_env, &job.payload)
            .await
            .map(|_| ())
    } else {
        let load = match &wiki.load {
            Some(probe) => model_load(probe.as_ref()).await,
            None => None,
        };
        let budget = if env.settings.backfill_enabled {
            crate::wiki_builder::backfill_budget(
                load,
                env.settings.backfill_max,
                env.settings.model_slots,
            )
        } else {
            kernel::wiki::builder::BackfillBudget::flat(0)
        };
        handle_wiki_backfill(&wiki_env, budget).await.map(|_| ())
    }
}

async fn dispatch_recompile<Q: JobQueue>(
    queue: &Q,
    env: &JobEnv,
    job: &ClaimedJob,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let wiki = env.wiki.as_ref().ok_or(WorkerError::MissingDependencies {
        role: ProcessRole::WikiBuilder,
    })?;
    let store = wiki
        .recompile
        .as_ref()
        .ok_or_else(|| WorkerError::Unhandled {
            name: job.name.to_string(),
        })?;
    let recompile = crate::wiki_builder::RecompileEnv {
        store: store.as_ref(),
        provider: wiki.provider.as_ref(),
        queue,
        model: &wiki.model,
        now: Utc::now(),
        cancel,
    };
    crate::wiki_builder::handle_recompile_wiki(&recompile, &job.payload).await
}

async fn dispatch_consolidate<Q: JobQueue>(
    queue: &Q,
    env: &JobEnv,
    job: &ClaimedJob,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let store = env
        .wiki
        .as_ref()
        .and_then(|wiki| wiki.consolidate.as_ref())
        .ok_or_else(|| WorkerError::Unhandled {
            name: job.name.to_string(),
        })?;
    handle_consolidate_wiki(store.as_ref(), queue, &job.payload, Utc::now(), cancel)
        .await
        .map(|_| ())
}

async fn dispatch_unembedded<Q: JobQueue>(
    queue: &Q,
    env: &JobEnv,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let store = env
        .unembedded
        .as_ref()
        .ok_or(WorkerError::MissingMemoryJobs)?;
    crate::embedding_sweep::handle_enqueue_unembedded(
        store.as_ref(),
        queue,
        env.settings.sweep_limit,
        env.settings.sweep_max_attempts,
        cancel,
    )
    .await
    .map(|_| ())
}

enum Sweep {
    Unchunked,
    StaleWiki,
}

async fn dispatch_sweep<Q: JobQueue>(
    queue: &Q,
    env: &JobEnv,
    sweep: Sweep,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    if matches!(sweep, Sweep::StaleWiki) && !env.wiki_enabled {
        return Ok(());
    }
    let store = env
        .memory_jobs
        .as_ref()
        .ok_or(WorkerError::MissingMemoryJobs)?;
    let store = store.as_ref();
    let settings = env.settings;
    match sweep {
        Sweep::Unchunked => crate::embedding_sweep::handle_enqueue_unchunked(
            store,
            queue,
            env.embed,
            settings.chunk_backfill_limit,
            settings.sweep_max_attempts,
            cancel,
        )
        .await
        .map(|_| ()),
        Sweep::StaleWiki => crate::embedding_sweep::handle_enqueue_stale_wiki(
            store,
            queue,
            env.wiki_enabled,
            settings.wiki_sweep_limit,
            cancel,
        )
        .await
        .map(|_| ()),
    }
}

async fn dispatch_entities<Q: JobQueue>(
    queue: &Q,
    env: &JobEnv,
    job: &ClaimedJob,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    if matches!(job.name, JobName::Fixed(FixedJob::EnqueueStaleEntities)) && !env.wiki_enabled {
        return Ok(());
    }
    let entity = env
        .entity
        .as_ref()
        .ok_or(WorkerError::MissingDependencies {
            role: ProcessRole::EntityBuilder,
        })?;
    let entity_env = EntityBuilderEnv {
        store: entity.store.as_ref(),
        provider: entity.provider.as_ref(),
        queue,
        model: &entity.model,
        settings: &entity.settings,
        now: Utc::now(),
        cancel,
    };
    match &job.name {
        JobName::Fixed(FixedJob::ExtractEntities) => {
            handle_extract_entities(&entity_env, &job.payload)
                .await
                .map(|_| ())
        }
        JobName::Fixed(FixedJob::EnqueueEntityBackfill) => {
            handle_entity_backfill(&entity_env, entity_budget(env).await)
                .await
                .map(|_| ())
        }
        JobName::Fixed(FixedJob::EnqueueStaleEntities) => {
            handle_enqueue_stale_entities(&entity_env, env.settings.wiki_sweep_limit)
                .await
                .map(|_| ())
        }
        JobName::Fixed(_) | JobName::Heartbeat { .. } => Err(WorkerError::Unhandled {
            name: job.name.to_string(),
        }),
    }
}

async fn entity_budget(env: &JobEnv) -> kernel::wiki::builder::BackfillBudget {
    if !env.settings.backfill_enabled {
        return kernel::wiki::builder::BackfillBudget::flat(0);
    }
    let load = match env.wiki.as_ref().and_then(|wiki| wiki.load.as_ref()) {
        Some(probe) => model_load(probe.as_ref()).await,
        None => None,
    };
    crate::wiki_builder::backfill_budget(load, env.settings.backfill_max, env.settings.model_slots)
}

async fn model_load(probe: &dyn kernel::model::ModelLoadProbe) -> Option<kernel::model::ModelLoad> {
    match probe.load().await {
        Ok(load) => Some(load),
        Err(error) => {
            tracing::warn!(
                "the model load is unreadable, so the wiki backfill stays at one job: {error}"
            );
            None
        }
    }
}
