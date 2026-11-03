use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use kernel::memory::ConversationWindow;
use kernel::memory::PendingConversation;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;
use wire::JsonObject;

use super::SummarizeEnv;
use super::leaf::count_of;
use super::llm::cancelled;
use super::settings::SummaryTrigger;
use crate::error::WorkerError;

const RECENT_IDLE_PRIORITY: i16 = -20;
const RECENT_STALE_PRIORITY: i16 = -10;
const IDLE_PRIORITY: i16 = 0;
const STALE_PRIORITY: i16 = 10;
const RECENT: TimeDelta = TimeDelta::hours(24);
const SUMMARIZE_ATTEMPTS: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummaryHealth {
    pub eligible: u64,
    pub active_tail: u64,
    pub below_floor: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SummarizePayload {
    pub(super) conversation_id: Uuid,
    pub(super) trigger_type: Option<SummaryTrigger>,
}

pub(super) fn effective_trigger(trigger: Option<SummaryTrigger>) -> SummaryTrigger {
    match trigger {
        Some(trigger) => trigger,
        None => SummaryTrigger::Threshold,
    }
}

pub(super) fn read_summarize(payload: &JsonObject) -> Result<SummarizePayload, WorkerError> {
    let value = serde_json::to_value(payload).map_err(WorkerError::summary_payload)?;
    serde_json::from_value(value).map_err(WorkerError::summary_payload)
}

pub fn summary_health(
    pending: &[PendingConversation],
    settings: &super::SummarySettings,
) -> Result<SummaryHealth, WorkerError> {
    let batch = u64::from(settings.leaf_batch);
    let floor = u64::from(settings.idle_floor());
    if batch == 0 {
        return Err(WorkerError::SummarySettings {
            message: "leaf batch must be at least 1".to_owned(),
        });
    }
    let mut health = SummaryHealth {
        eligible: 0,
        active_tail: 0,
        below_floor: 0,
    };
    for conversation in pending {
        let count = u64::from(conversation.unsummarized);
        let full = count / batch * batch;
        let rest = count % batch;
        health.eligible = health
            .eligible
            .checked_add(full)
            .ok_or(WorkerError::Count)?;
        if rest >= floor {
            health.active_tail = health
                .active_tail
                .checked_add(rest)
                .ok_or(WorkerError::Count)?;
        } else {
            health.below_floor = health
                .below_floor
                .checked_add(rest)
                .ok_or(WorkerError::Count)?;
        }
    }
    Ok(health)
}

pub async fn handle_enqueue_idle(env: &SummarizeEnv<'_>) -> Result<u64, WorkerError> {
    sweep(env, SummaryTrigger::SessionIdle, idle_due).await
}

pub async fn handle_enqueue_stale(env: &SummarizeEnv<'_>) -> Result<u64, WorkerError> {
    sweep(env, SummaryTrigger::SessionStale, stale_due).await
}

async fn sweep(
    env: &SummarizeEnv<'_>,
    trigger: SummaryTrigger,
    due: fn(&PendingConversation, &SummarizeEnv<'_>) -> bool,
) -> Result<u64, WorkerError> {
    cancelled(env)?;
    let pending = env.store.pending().await?;
    let mut enqueued = 0u64;
    for conversation in pending.iter().filter(|conversation| due(conversation, env)) {
        enqueue(
            env,
            conversation.id,
            trigger,
            conversation.latest_message_at,
        )
        .await?;
        enqueued = enqueued.checked_add(1).ok_or(WorkerError::Count)?;
    }
    Ok(enqueued)
}

pub async fn handle_reap(env: &SummarizeEnv<'_>) -> Result<u64, WorkerError> {
    cancelled(env)?;
    let flagged = env.queue.flag_stuck().await.map_err(WorkerError::queue)?;
    if flagged > 0 {
        tracing::warn!("[reap-dead-jobs] flagged {flagged} job(s) that ran out of attempts");
    }
    Ok(flagged)
}

pub(super) async fn enqueue_again(
    env: &SummarizeEnv<'_>,
    window: &ConversationWindow,
    trigger: SummaryTrigger,
) -> Result<(), WorkerError> {
    enqueue(
        env,
        window.conversation.id,
        trigger,
        window.latest_message_at,
    )
    .await
}

pub(super) fn unfinished(
    window: &ConversationWindow,
    trigger: SummaryTrigger,
    env: &SummarizeEnv<'_>,
) -> Result<bool, WorkerError> {
    let available = count_of(window.messages.len())?;
    let leaves = count_of(window.leaves.len())?;
    let branches = count_of(window.branches.len())?;
    let full = available >= env.settings.leaf_batch;
    let partial = partial_allowed(
        trigger,
        window.latest_message_at,
        available,
        env.settings.leaf_batch,
        env,
    );
    let more_leaves = leaves >= env.settings.min_leaves_for_branch;
    let more_branches = branches >= env.settings.min_branches_for_root;
    Ok(full || partial || more_leaves || more_branches)
}

pub(super) fn partial_allowed(
    trigger: SummaryTrigger,
    latest: Option<DateTime<Utc>>,
    count: u32,
    batch: u32,
    env: &SummarizeEnv<'_>,
) -> bool {
    if count == 0 || count >= batch {
        return false;
    }
    match trigger {
        SummaryTrigger::Threshold => false,
        SummaryTrigger::Explicit => true,
        SummaryTrigger::SessionIdle => {
            aged(latest, env.now, env.settings.idle) && count >= env.settings.idle_floor()
        }
        SummaryTrigger::SessionStale => {
            aged(latest, env.now, env.settings.stale) && count >= env.settings.stale_min_batch
        }
    }
}

fn idle_due(conversation: &PendingConversation, env: &SummarizeEnv<'_>) -> bool {
    let count = conversation.unsummarized;
    let batch = env.settings.leaf_batch;
    count >= batch
        || partial_allowed(
            SummaryTrigger::SessionIdle,
            conversation.latest_message_at,
            count,
            batch,
            env,
        )
}

fn stale_due(conversation: &PendingConversation, env: &SummarizeEnv<'_>) -> bool {
    conversation.unsummarized >= env.settings.stale_min_batch
        && aged(conversation.latest_message_at, env.now, env.settings.stale)
}

async fn enqueue(
    env: &SummarizeEnv<'_>,
    id: Uuid,
    trigger: SummaryTrigger,
    latest: Option<DateTime<Utc>>,
) -> Result<(), WorkerError> {
    env.queue
        .enqueue(summarize_job(
            id,
            trigger,
            priority_of(trigger, latest, env.now),
            env.now,
        )?)
        .await
        .map(|_| ())
        .map_err(WorkerError::queue)
}

fn summarize_job(
    id: Uuid,
    trigger: SummaryTrigger,
    priority: i16,
    now: DateTime<Utc>,
) -> Result<Job, WorkerError> {
    let payload = SummarizePayload {
        conversation_id: id,
        trigger_type: Some(trigger),
    };
    Ok(Job {
        name: JobName::Fixed(FixedJob::SummarizeConversation),
        payload: JsonObject::from_serialize(&payload).map_err(WorkerError::summary_payload)?,
        key: Some(format!("compact-{id}")),
        key_mode: JobKeyMode::Arm,
        priority,
        run_at: Some(now),
        max_attempts: Some(SUMMARIZE_ATTEMPTS),
    })
}

fn priority_of(trigger: SummaryTrigger, latest: Option<DateTime<Utc>>, now: DateTime<Utc>) -> i16 {
    let recent = message_age(latest, now).is_some_and(|age| age < RECENT);
    match (trigger, recent) {
        (SummaryTrigger::SessionStale, true) => RECENT_STALE_PRIORITY,
        (SummaryTrigger::SessionStale, false) => STALE_PRIORITY,
        (
            SummaryTrigger::Threshold | SummaryTrigger::SessionIdle | SummaryTrigger::Explicit,
            true,
        ) => RECENT_IDLE_PRIORITY,
        (
            SummaryTrigger::Threshold | SummaryTrigger::SessionIdle | SummaryTrigger::Explicit,
            false,
        ) => IDLE_PRIORITY,
    }
}

fn aged(latest: Option<DateTime<Utc>>, now: DateTime<Utc>, minimum: TimeDelta) -> bool {
    match message_age(latest, now) {
        Some(age) => age >= minimum,
        None => false,
    }
}

fn message_age(latest: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<TimeDelta> {
    latest.map(|at| now.signed_duration_since(at))
}
