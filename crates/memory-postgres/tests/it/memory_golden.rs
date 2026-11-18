use jiff::Timestamp;
use std::collections::{BTreeMap, BTreeSet};

use kernel::memory::{CaptureBatchRequest, CaptureResult};
use memory_postgres::{CaptureOptions, PostgresMemory};
use serde::Deserialize;
use sqlx::{PgPool, types::Json};
use wire::JsonObject;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Deserialize)]
struct Recording {
    results: Vec<Scenario>,
    conversations: Vec<RecordedConversation>,
    messages: Vec<RecordedMessage>,
    tags: Vec<Tag>,
}

#[derive(Deserialize)]
struct Scenario {
    label: String,
    batch: CaptureBatchRequest,
    opts: Options,
    result: CaptureResult,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
    owner_user_id: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, sqlx::FromRow)]
struct Conversation {
    session_key: String,
    agent: String,
    channel: String,
    title: Option<String>,
    settings: Json<JsonObject>,
    task_id: Option<String>,
    active: bool,
    owner_user_id: Option<String>,
}

fn caller_owned(recorded: Option<&str>) -> &str {
    recorded.unwrap_or(testkit::OWNER)
}

#[derive(Deserialize)]
struct RecordedConversation {
    id: String,
    #[serde(flatten)]
    row: Conversation,
}

#[serde_with::serde_as]
#[derive(Debug, Deserialize, PartialEq, sqlx::FromRow)]
struct Message {
    agent: String,
    channel: String,
    role: Json<Role>,
    content: String,
    tool_name: Option<String>,
    tool_args: Option<Json<JsonObject>>,
    tool_result: Option<String>,
    metadata: Json<JsonObject>,

    #[serde_as(as = "wire::time::Rfc3339")]
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    created_at: Timestamp,
    owner_user_id: Option<String>,
    content_hash: Option<String>,
}

#[derive(Deserialize)]
struct RecordedMessage {
    conversation_id: String,
    #[serde(flatten)]
    row: Message,
}

#[derive(Debug, Deserialize, PartialEq, Eq, sqlx::FromRow)]
struct Tag {
    entity_type: String,
    key: String,
    value: String,
    state: String,
    source: String,
}

fn recording() -> Result<Recording, serde_json::Error> {
    serde_json::from_str(include_str!(
        "../../../testkit/tests/fixtures/memory/capture-batch.json"
    ))
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn capture_batch_golden(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let recording = recording()?;
    let mut conversations = BTreeMap::new();
    let mut events = BTreeSet::new();
    let mut skipped_conversations = BTreeSet::new();
    for case in &recording.results {
        let options = CaptureOptions::owned_by(&kernel::owner::Owner::new(
            case.opts
                .owner_user_id
                .clone()
                .unwrap_or_else(|| testkit::OWNER.to_owned()),
        )?);
        if case.batch.channel.is_none() {
            skipped_conversations.insert(&case.result.conversation_id);
            continue;
        }
        let actual = memory.capture_batch(&case.batch, &options).await?;
        assert_eq!(actual.ok, case.result.ok, "{}", case.label);
        assert_eq!(actual.inserted, case.result.inserted, "{}", case.label);
        assert_eq!(actual.skipped, case.result.skipped, "{}", case.label);
        conversations.insert(case.result.conversation_id.clone(), actual.conversation_id);
        for message in &case.batch.messages {
            events.insert((
                case.result.conversation_id.clone(),
                message.event_id.clone(),
            ));
        }
    }
    for expected in &recording.conversations {
        if skipped_conversations.contains(&expected.id) {
            continue;
        }
        let id = conversations
            .get(&expected.id)
            .ok_or("unreplayed conversation")?;
        let mut actual: Conversation = sqlx::query_as(
            "SELECT session_key, agent, channel, title, settings, task_id::text, active, owner_user_id
             FROM rh_conversations WHERE id = $1::text::uuid",
        ).bind(id).fetch_one(&pool).await?;
        assert_eq!(
            actual.owner_user_id.as_deref(),
            Some(caller_owned(expected.row.owner_user_id.as_deref()))
        );
        actual.owner_user_id.clone_from(&expected.row.owner_user_id);
        assert_eq!(actual, expected.row);
    }
    let mut checked = 0i64;
    for expected in &recording.messages {
        let event_id = expected
            .row
            .metadata
            .0
            .get("event_id")
            .and_then(|value| value.as_str())
            .ok_or("recorded event id")?;
        if !events.contains(&(expected.conversation_id.clone(), event_id.to_owned())) {
            continue;
        }
        let id = conversations
            .get(&expected.conversation_id)
            .ok_or("unreplayed message")?;
        let mut actual: Message = sqlx::query_as(
            "SELECT agent, channel, to_json(role) AS role, content, tool_name, tool_args, tool_result, metadata,
                    created_at, owner_user_id, content_hash
             FROM rh_messages WHERE conversation_id = $1::text::uuid AND metadata->>'event_id' = $2",
        ).bind(id).bind(event_id).fetch_one(&pool).await?;
        assert_eq!(
            actual.owner_user_id.as_deref(),
            Some(caller_owned(expected.row.owner_user_id.as_deref()))
        );
        actual.owner_user_id.clone_from(&expected.row.owner_user_id);
        assert_eq!(actual, expected.row);
        checked += 1;
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_messages")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, checked);
    let tags: Vec<Tag> = sqlx::query_as(
        "SELECT entity_type, key, value, state, source FROM rh_tags ORDER BY created_at, id",
    )
    .fetch_all(&pool)
    .await?;
    let kept: Vec<&Tag> = recording
        .tags
        .iter()
        .filter(|tag| tag.source != "rule")
        .collect();
    assert_eq!(tags.iter().collect::<Vec<_>>(), kept);
    let jobs: Vec<(String, i64)> = sqlx::query_as(
        "SELECT t.identifier, count(*) FROM graphile_worker._private_jobs j
          JOIN graphile_worker._private_tasks t ON t.id = j.task_id
         GROUP BY t.identifier ORDER BY t.identifier",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        jobs,
        [
            ("embed-target".to_owned(), count),
            ("file-conversation".to_owned(), 2),
            ("title-conversation".to_owned(), 2),
        ]
    );
    Ok(())
}
