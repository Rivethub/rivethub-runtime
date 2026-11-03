mod leaf;
mod llm;
mod opening;
mod prompt;
mod rollup;
mod settings;
mod sweep;
mod title;

use kernel::memory::ConversationWindow;
use kernel::memory::HEARTBEAT_SESSION_PREFIX;
use kernel::memory::SummaryStore;
use kernel::model::ModelProvider;
use kernel::queue::JobQueue;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;

pub use settings::SummaryParts;
pub use settings::SummarySettings;
pub use settings::SummaryTrigger;
pub use sweep::SummaryHealth;
pub use sweep::handle_enqueue_idle;
pub use sweep::handle_enqueue_stale;
pub use sweep::handle_reap;
pub use sweep::summary_health;

pub(crate) use llm::llm_error;

pub use prompt::BRANCH_SYSTEM_PROMPT;
pub use prompt::LEAF_SYSTEM_PROMPT;
pub use prompt::ROOT_SYSTEM_PROMPT;
pub use title::Named;
pub use title::TITLE_LEAVES;
pub use title::TITLE_SYSTEM_PROMPT;
pub use title::TitleCall;

use crate::error::WorkerError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummarizeOutcome {
    pub summaries: u32,
    pub reenqueued: bool,
}

#[expect(
    missing_debug_implementations,
    reason = "the summary store, model provider and job queue are trait objects"
)]
pub struct SummarizeEnv<'a> {
    pub store: &'a dyn SummaryStore,
    pub provider: &'a dyn ModelProvider,
    pub queue: &'a dyn JobQueue,
    pub settings: &'a SummarySettings,
    pub now: chrono::DateTime<chrono::Utc>,
    pub cancel: &'a CancellationToken,
}

pub async fn handle_summarize(
    env: &SummarizeEnv<'_>,
    payload: &JsonObject,
) -> Result<SummarizeOutcome, WorkerError> {
    llm::cancelled(env)?;
    let request = sweep::read_summarize(payload)?;
    let trigger = sweep::effective_trigger(request.trigger_type);
    let Some(mut window) = env.store.conversation(request.conversation_id).await? else {
        tracing::info!(
            "conversation {} is gone — dropping its summary job",
            id_prefix(request.conversation_id)
        );
        return Ok(SummarizeOutcome {
            summaries: 0,
            reenqueued: false,
        });
    };
    if window
        .conversation
        .session_key
        .starts_with(HEARTBEAT_SESSION_PREFIX)
    {
        tracing::info!("skipped heartbeat conversation {}", window.conversation.id);
        return Ok(SummarizeOutcome {
            summaries: 0,
            reenqueued: false,
        });
    }
    let written = fold_levels(env, &mut window, trigger).await?;
    let reenqueued = reenqueue(env, &window, trigger, written).await?;
    if written > 0 {
        tracing::info!(
            "summarized {}: {written} summaries",
            id_prefix(window.conversation.id)
        );
    }
    Ok(SummarizeOutcome {
        summaries: written,
        reenqueued,
    })
}

async fn fold_levels(
    env: &SummarizeEnv<'_>,
    window: &mut ConversationWindow,
    trigger: SummaryTrigger,
) -> Result<u32, WorkerError> {
    let leaves = leaf::write_leaves(env, window, trigger).await?;
    let branches = rollup::write_branch(env, window).await?;
    let roots = rollup::write_root(env, window).await?;
    leaves
        .checked_add(branches)
        .and_then(|total| total.checked_add(roots))
        .ok_or(WorkerError::Count)
}

async fn reenqueue(
    env: &SummarizeEnv<'_>,
    window: &ConversationWindow,
    trigger: SummaryTrigger,
    written: u32,
) -> Result<bool, WorkerError> {
    if written == 0 || !sweep::unfinished(window, trigger, env)? {
        return Ok(false);
    }
    sweep::enqueue_again(env, window, trigger).await?;
    Ok(true)
}

fn id_prefix(id: uuid::Uuid) -> String {
    let text = id.to_string();
    match text.get(..8) {
        Some(prefix) => prefix.to_owned(),
        None => text,
    }
}
