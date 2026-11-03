use kernel::memory::ConversationHead;
use kernel::memory::ConversationWindow;
use kernel::memory::NewSummary;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryNode;
use kernel::memory::SummarySource;
use kernel::memory::SummaryWrite;
use kernel::memory::WindowMessage;
use kernel::model::FinishReason;
use uuid::Uuid;

use super::SummarizeEnv;
use super::llm::chat;
use super::llm::require_text;
use super::opening::work_first;
use super::prompt::LEAF_SYSTEM_PROMPT;
use super::prompt::leaf_user;
use super::settings::SummaryTrigger;
use crate::error::WorkerError;

pub(super) async fn write_leaves(
    env: &SummarizeEnv<'_>,
    window: &mut ConversationWindow,
    trigger: SummaryTrigger,
) -> Result<u32, WorkerError> {
    let mut batch = env.settings.leaf_batch;
    let mut rounds = 0u32;
    let mut written = 0u32;
    while rounds < env.settings.max_leaf_rounds {
        let available = count_of(window.messages.len())?;
        if available == 0 {
            break;
        }
        let Some(take) = take_count(env, window, trigger, available, batch) else {
            break;
        };
        let user = {
            let messages = window
                .messages
                .get(..usize_of(take)?)
                .ok_or(WorkerError::Count)?;
            leaf_user(&window.conversation, messages)?
        };
        let response = chat(env, LEAF_SYSTEM_PROMPT, &user, env.settings.leaf_max_tokens).await?;
        if response.finish_reason == Some(FinishReason::Length) {
            batch = shrink_batch(batch)?;
            continue;
        }
        let content = work_first(&require_text(&response)?, &window.conversation.agent);
        let taken = take_front(&mut window.messages, usize_of(take)?)?;
        let summary = leaf_record(env, &window.conversation, &taken, content)?;
        let conversation_id = summary.conversation_id;
        let node = node_from(&summary);
        let stored = env.store.write_summary(summary).await?;
        if stored == SummaryWrite::FirstLeaf {
            enqueue_filing(env, conversation_id).await?;
            super::title::title_first_leaf(env, &window.conversation, &node.content).await?;
        }
        window.leaves.push(node);
        written = written.checked_add(1).ok_or(WorkerError::Count)?;
        rounds = rounds.checked_add(1).ok_or(WorkerError::Count)?;
    }
    Ok(written)
}

fn take_count(
    env: &SummarizeEnv<'_>,
    window: &ConversationWindow,
    trigger: SummaryTrigger,
    available: u32,
    batch: u32,
) -> Option<u32> {
    if available >= batch {
        return Some(batch);
    }
    if super::sweep::partial_allowed(trigger, window.latest_message_at, available, batch, env) {
        Some(available)
    } else {
        None
    }
}

fn leaf_record(
    env: &SummarizeEnv<'_>,
    head: &ConversationHead,
    messages: &[WindowMessage],
    content: String,
) -> Result<NewSummary, WorkerError> {
    let start = messages.first().ok_or(WorkerError::Count)?.created_at;
    let end = messages.last().ok_or(WorkerError::Count)?.created_at;
    let mut sources = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        sources.push(SummarySource {
            message_id: message.id,
            ordinal: i32::try_from(index).map_err(|_| WorkerError::Count)?,
        });
    }
    Ok(NewSummary {
        id: Uuid::new_v4(),
        conversation_id: head.id,
        kind: SummaryKind::Leaf,
        depth: SummaryKind::Leaf.depth(),
        content,
        message_count: count_of(messages.len())?,
        earliest_at: start,
        latest_at: end,
        model: env.settings.model.clone(),
        pipeline_version: env.settings.pipeline_version,
        created_at: env.now,
        sources,
        child_ids: Vec::new(),
    })
}

pub(super) fn node_from(summary: &NewSummary) -> SummaryNode {
    SummaryNode {
        id: summary.id,
        kind: summary.kind,
        content: summary.content.clone(),
        message_count: summary.message_count,
        earliest_at: summary.earliest_at,
        latest_at: summary.latest_at,
        depth: summary.depth,
    }
}

pub(super) fn take_front<T>(items: &mut Vec<T>, count: usize) -> Result<Vec<T>, WorkerError> {
    if count > items.len() {
        return Err(WorkerError::Count);
    }
    let rest = items.split_off(count);
    Ok(std::mem::replace(items, rest))
}

pub(super) fn count_of(value: usize) -> Result<u32, WorkerError> {
    u32::try_from(value).map_err(|_| WorkerError::Count)
}

pub(super) fn usize_of(value: u32) -> Result<usize, WorkerError> {
    usize::try_from(value).map_err(|_| WorkerError::Count)
}

async fn enqueue_filing(env: &SummarizeEnv<'_>, conversation_id: Uuid) -> Result<(), WorkerError> {
    env.queue
        .enqueue(crate::filing::file_conversation_job(conversation_id)?)
        .await
        .map(|_| ())
        .map_err(WorkerError::queue)
}

fn shrink_batch(batch: u32) -> Result<u32, WorkerError> {
    let next = batch / 2;
    if next == 0 || next >= batch {
        return Err(WorkerError::LeafShrink);
    }
    Ok(next)
}
