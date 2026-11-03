#![cfg(test)]

#[expect(
    unreachable_pub,
    reason = "the harness is a private module of each test binary"
)]
#[expect(
    dead_code,
    reason = "each test binary uses a different subset of the harness"
)]
mod common;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use common::Phase;
use common::Queue;
use kernel::memory::ConversationHead;
use kernel::memory::ConversationWindow;
use kernel::memory::NewSummary;
use kernel::memory::PendingConversation;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryNode;
use kernel::memory::SummaryStore;
use kernel::memory::SummaryStoreError;
use kernel::memory::SummaryWrite;
use kernel::memory::WindowMessage;
use kernel::model::Embedding;
use kernel::model::FinishReason;
use kernel::model::LlmResponse;
use kernel::model::LlmResponseKind;
use kernel::model::ModelProvider;
use kernel::model::ModelProviderError;
use kernel::model::ModelRequest;
use kernel::model::ModelStreamItem;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobId;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::queue::JobQueueError;
use kernel::queue::WorkerRole;
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::message::MessageContent;
use wire::message::Role;
use workers::BRANCH_SYSTEM_PROMPT;
use workers::LEAF_SYSTEM_PROMPT;
use workers::ROOT_SYSTEM_PROMPT;
use workers::SummarizeEnv;
use workers::SummarizeOutcome;
use workers::SummaryHealth;
use workers::SummaryParts;
use workers::SummarySettings;
use workers::SummaryTrigger;
use workers::TITLE_SYSTEM_PROMPT;
use workers::handle_enqueue_idle;
use workers::handle_enqueue_stale;
use workers::handle_reap;
use workers::handle_summarize;
use workers::summary_health;

const LEAF_TEXT: &str = "The team decided to deploy the AcmeKit service with a blue-green rollout. The release branch is cut on Mondays, the deploy script runs from the release branch, and a health check on staging.example.com gates the traffic switch. Rollback is a traffic switch back to the previous colour.";
const BRANCH_TEXT: &str = "Branch summary: several deploy sessions for AcmeKit settled on blue-green releases with a health-check gate.";
const TITLE_ANSWER: &str = r#"{"title": "Plan blue-green AcmeKit deploys"}"#;
const ROOT_TEXT: &str = "Root summary: the whole AcmeKit deploy conversation, from the first release plan to the documented rollback.";

const REPLAYED: &[&str] = &[
    "leaf once a full window exists",
    "leaves → branches → root with small batches",
    "truncated answer shrinks the leaf batch",
    "failing LLM fails the job; heartbeat never summarized",
    "permanent failure → dead → rests an hour → revived",
    "below the floor until idle/stale",
];

const SKIPPED: &[(&str, &str)] = &[
    (
        "synthesize-tool-call",
        "no recording of this job; synthesis has dedicated database fixtures and provider retry tests",
    ),
    (
        "WIKI_EXTRACT_SYSTEM_PROMPT",
        "wiki extraction belongs to the wiki builder",
    ),
    (
        "WIKI_RECOMPILE_SYSTEM_PROMPT",
        "wiki recompile belongs to the wiki builder",
    ),
    (
        "TAG_SYSTEM_PROMPT",
        "tag suggestion belongs to the categorizer",
    ),
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptHashes {
    #[serde(rename = "LEAF_SYSTEM_PROMPT")]
    leaf: String,
    #[serde(rename = "BRANCH_SYSTEM_PROMPT")]
    branch: String,
    #[serde(rename = "ROOT_SYSTEM_PROMPT")]
    root: String,
    #[serde(rename = "WIKI_EXTRACT_SYSTEM_PROMPT")]
    wiki_extract: String,
    #[serde(rename = "WIKI_RECOMPILE_SYSTEM_PROMPT")]
    wiki_recompile: String,
    #[serde(rename = "TAG_SYSTEM_PROMPT")]
    tag: String,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    prompts_sha256: PromptHashes,
    scenarios: Vec<Scenario>,
}

#[derive(Debug, Deserialize)]
struct Scenario {
    name: String,
    steps: Vec<Step>,
    #[serde(default)]
    chat_calls: Option<ChatCalls>,
    #[serde(default)]
    tables: Tables,
    #[serde(default)]
    logs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ChatCalls {
    Calls(Vec<ChatCall>),
    Count(u64),
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCall {
    prompt: String,
    user: String,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Step {
    label: String,
    result: StepResult,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StepResult {
    Count(u64),
    Health(HealthSnap),
    Rows(Vec<wire::Extra>),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct HealthSnap {
    eligible: u64,
    #[serde(rename = "activeTail")]
    active_tail: u64,
    #[serde(rename = "belowFloor")]
    below_floor: u64,
}

#[derive(Debug, Default, Deserialize)]
struct Tables {
    #[serde(default, rename = "ros_summaries")]
    summaries: Vec<SummaryRow>,
    #[serde(default, rename = "ros_jobs")]
    jobs: Vec<JobRow>,
}

#[derive(Debug, Deserialize)]
struct SummaryRow {
    conversation_id: Uuid,
    parent_id: Option<Uuid>,
    depth: i32,
    content: String,
    kind: String,
    message_count: u32,
    earliest_at: DateTime<Utc>,
    latest_at: DateTime<Utc>,
    model: String,
    pipeline_version: i32,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct JobRow {
    task: String,
    attempts: u32,
    max_attempts: u32,
    state: String,
    last_error: String,
    #[serde(default)]
    payload: Option<String>,
}

#[derive(Debug)]
struct Captured {
    kind: String,
    user: String,
    max_tokens: Option<u32>,
    model: String,
}

struct Scripted {
    failures: Mutex<Vec<u16>>,
    truncations: Mutex<u32>,
    calls: Mutex<Vec<Captured>>,
}

struct MemStore {
    windows: Mutex<Vec<ConversationWindow>>,
    written: Mutex<Vec<NewSummary>>,
    retitled: Mutex<Vec<(Uuid, String)>>,
}

#[derive(Debug)]
struct Held {
    id: JobId,
    job: Job,
    attempts: u32,
    last_error: Option<String>,
    claimed: bool,
}

#[derive(Debug)]
struct Seen {
    notifications: tokio::sync::watch::Sender<()>,
    jobs: Mutex<Vec<Held>>,
    dead: Mutex<Vec<JobName>>,
    next_id: std::sync::atomic::AtomicI64,
}

impl Default for Seen {
    fn default() -> Self {
        Self {
            notifications: tokio::sync::watch::channel(()).0,
            jobs: Mutex::new(Vec::new()),
            dead: Mutex::new(Vec::new()),
            next_id: std::sync::atomic::AtomicI64::new(1),
        }
    }
}

fn fixture() -> Fixture {
    let path = testkit::recording("memory-sqlite/summarizer.json");
    let text = std::fs::read_to_string(path).expect("summarizer fixture");
    serde_json::from_str(&text).expect("summarizer json")
}

fn recorded_name(index: usize) -> &'static str {
    REPLAYED.get(index).copied().expect("recorded scenario")
}

fn scenario<'a>(fixture: &'a Fixture, name: &str) -> &'a Scenario {
    fixture
        .scenarios
        .iter()
        .find(|scenario| scenario.name == name)
        .expect("scenario")
}

fn chat_calls(scenario: &Scenario) -> Vec<ChatCall> {
    match &scenario.chat_calls {
        Some(ChatCalls::Calls(calls)) => calls.clone(),
        Some(ChatCalls::Count(_)) | None => Vec::new(),
    }
}

fn health_steps(scenario: &Scenario) -> Vec<HealthSnap> {
    scenario
        .steps
        .iter()
        .filter_map(|step| match &step.result {
            StepResult::Health(health) => Some(health.clone()),
            StepResult::Count(_) | StepResult::Rows(_) => None,
        })
        .collect()
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .expect("timestamp")
        .with_timezone(&Utc)
}

fn settings() -> SummarySettings {
    SummarySettings::standard("fake-chat").expect("settings")
}

fn small_settings() -> SummarySettings {
    SummarySettings::from_parts(small_parts()).expect("small settings")
}

fn small_parts() -> SummaryParts {
    SummaryParts {
        model: "fake-chat".to_owned(),
        leaf_batch: 5,
        branch_batch: 2,
        root_batch: 5,
        min_leaves_for_branch: 2,
        min_branches_for_root: 2,
        idle_minutes: 15,
        stale_minutes: 5_760,
        stale_min_batch: 2,
        max_leaf_rounds: 10,
        pipeline_version: 5,
    }
}

fn messages(start: DateTime<Utc>, count: usize) -> Vec<WindowMessage> {
    (0..count)
        .map(|index| WindowMessage {
            id: Uuid::new_v4(),
            agent: "rivet".to_owned(),
            role: if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            content: format!("message number {index} about deploying the AcmeKit service"),
            tool: None,
            created_at: start
                .checked_add_signed(Duration::minutes(i64::try_from(index).expect("index")))
                .expect("message time"),
        })
        .collect()
}

fn window(
    id: Uuid,
    session: &str,
    title: &str,
    messages: Vec<WindowMessage>,
) -> ConversationWindow {
    let latest_message_at = messages.last().map(|message| message.created_at);
    ConversationWindow {
        conversation: ConversationHead {
            id,
            session_key: session.to_owned(),
            agent: "rivet".to_owned(),
            channel: "cli".to_owned(),
            title: Some(title.to_owned()),
        },
        messages,
        leaves: Vec::new(),
        branches: Vec::new(),
        latest_message_at,
    }
}

fn cancel() -> CancellationToken {
    CancellationToken::new()
}

fn payload_of(id: Uuid, trigger: SummaryTrigger) -> wire::JsonObject {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Body {
        conversation_id: Uuid,
        trigger_type: SummaryTrigger,
    }
    wire::JsonObject::from_serialize(&Body {
        conversation_id: id,
        trigger_type: trigger,
    })
    .expect("payload")
}

fn read_trigger(payload: &wire::JsonObject) -> SummaryTrigger {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Body {
        trigger_type: SummaryTrigger,
    }
    let value = serde_json::to_value(payload).expect("payload value");
    serde_json::from_value::<Body>(value)
        .expect("trigger")
        .trigger_type
}

#[path = "summarizer/leaf.rs"]
mod leaf;
#[path = "summarizer/support.rs"]
mod support;
#[path = "summarizer/sweeps.rs"]
mod sweeps;
#[path = "summarizer/triggers.rs"]
mod triggers;

use support::*;
