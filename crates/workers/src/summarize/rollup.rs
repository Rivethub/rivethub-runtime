use kernel::memory::ConversationWindow;
use kernel::memory::NewSummary;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryNode;
use kernel::model::FinishReason;
use uuid::Uuid;

use super::SummarizeEnv;
use super::leaf::count_of;
use super::leaf::node_from;
use super::leaf::take_front;
use super::leaf::usize_of;
use super::llm::chat;
use super::llm::require_text;
use super::prompt::BRANCH_SYSTEM_PROMPT;
use super::prompt::ROOT_SYSTEM_PROMPT;
use super::prompt::branch_user;
use super::prompt::root_user;
use crate::error::WorkerError;

struct RollupSpec<'a> {
    kind: SummaryKind,
    minimum: u32,
    batch: u32,
    system: &'a str,
    max_tokens: u32,
}

pub(super) async fn write_branch(
    env: &SummarizeEnv<'_>,
    window: &mut ConversationWindow,
) -> Result<u32, WorkerError> {
    write_rollup(
        env,
        window,
        RollupSpec {
            kind: SummaryKind::Branch,
            minimum: env.settings.min_leaves_for_branch,
            batch: env.settings.branch_batch,
            system: BRANCH_SYSTEM_PROMPT,
            max_tokens: env.settings.branch_max_tokens,
        },
    )
    .await
}

pub(super) async fn write_root(
    env: &SummarizeEnv<'_>,
    window: &mut ConversationWindow,
) -> Result<u32, WorkerError> {
    write_rollup(
        env,
        window,
        RollupSpec {
            kind: SummaryKind::Root,
            minimum: env.settings.min_branches_for_root,
            batch: env.settings.root_batch,
            system: ROOT_SYSTEM_PROMPT,
            max_tokens: env.settings.root_max_tokens,
        },
    )
    .await
}

async fn write_rollup(
    env: &SummarizeEnv<'_>,
    window: &mut ConversationWindow,
    spec: RollupSpec<'_>,
) -> Result<u32, WorkerError> {
    let available = match spec.kind {
        SummaryKind::Branch => window.leaves.len(),
        SummaryKind::Root => window.branches.len(),
        SummaryKind::Leaf => return Err(WorkerError::Count),
    };
    if count_of(available)? < spec.minimum {
        return Ok(0);
    }
    let mut take = usize_of(spec.batch)?.min(available);
    let response = loop {
        let user = rollup_prompt(window, spec.kind, take)?;
        let response = chat(env, spec.system, &user, spec.max_tokens).await?;
        if response.finish_reason != Some(FinishReason::Length) {
            break response;
        }
        take = match take / 2 {
            0 => return Err(WorkerError::Truncated),
            fewer => fewer,
        };
    };
    let content = require_text(&response)?;
    let children = take_children(window, spec.kind, take)?;
    let summary = rollup_record(env, window, spec.kind, &children, content)?;
    let node = node_from(&summary);
    env.store.write_summary(summary).await?;
    if spec.kind == SummaryKind::Branch {
        window.branches.push(node);
    }
    Ok(1)
}

fn rollup_prompt(
    window: &ConversationWindow,
    kind: SummaryKind,
    take: usize,
) -> Result<String, WorkerError> {
    match kind {
        SummaryKind::Branch => {
            let nodes = window.leaves.get(..take).ok_or(WorkerError::Count)?;
            branch_user(&window.conversation, nodes)
        }
        SummaryKind::Root => {
            let nodes = window.branches.get(..take).ok_or(WorkerError::Count)?;
            root_user(&window.conversation, nodes)
        }
        SummaryKind::Leaf => Err(WorkerError::Count),
    }
}

fn take_children(
    window: &mut ConversationWindow,
    kind: SummaryKind,
    take: usize,
) -> Result<Vec<SummaryNode>, WorkerError> {
    match kind {
        SummaryKind::Branch => take_front(&mut window.leaves, take),
        SummaryKind::Root => take_front(&mut window.branches, take),
        SummaryKind::Leaf => Err(WorkerError::Count),
    }
}

fn rollup_record(
    env: &SummarizeEnv<'_>,
    window: &ConversationWindow,
    kind: SummaryKind,
    children: &[SummaryNode],
    content: String,
) -> Result<NewSummary, WorkerError> {
    let first = children.first().ok_or(WorkerError::Count)?;
    let last = children.last().ok_or(WorkerError::Count)?;
    let message_count = children
        .iter()
        .try_fold(0u32, |total, child| total.checked_add(child.message_count))
        .ok_or(WorkerError::Count)?;
    Ok(NewSummary {
        id: Uuid::new_v4(),
        conversation_id: window.conversation.id,
        kind,
        depth: kind.depth(),
        content,
        message_count,
        earliest_at: first.earliest_at,
        latest_at: last.latest_at,
        model: env.settings.model.clone(),
        pipeline_version: env.settings.pipeline_version,
        created_at: env.now,
        sources: Vec::new(),
        child_ids: children.iter().map(|child| child.id).collect(),
    })
}
