use axum::http::StatusCode;
use jiff::Timestamp;
use kernel::filing::FilingStore as _;
use kernel::filing::Refusal;
use kernel::filing::Rule;
use kernel::filing::Target;
use kernel::memory::MemoryStore;
use kernel::model::LlmResponse;
use kernel::model::LlmResponseKind;
use kernel::topics::TopicId;
use kernel::topics::TopicName;
use testkit::FakeProvider;
use testkit::Script;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::JsonObject;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::gateway::memory::MemorySummaryDagResponse;
use wire::gateway::topics::ConversationPlacementReply;
use wire::gateway::topics::PlacedBy;
use wire::gateway::topics::TopicRecord;
use wire::gateway::topics::TopicSessionList;
use wire::message::Role;
use wire::session_id::SessionId;
use workers::FilingEnv;
use workers::FilingPlan;
use workers::SessionTopics;

use super::Api;
use super::TestResult;

const RAIL: &str = "00000000-0000-4000-8000-000000000041";
const COUNTING: &str = "00000000-0000-4000-8000-000000000042";

#[tokio::test]
async fn a_first_leaf_files_by_folder_then_by_the_model() -> TestResult {
    let api = Api::new(true)?;
    let desktop: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"Desktop App","directory":{"kind":"shared","path":"/work/rhd"}}"#,
            StatusCode::CREATED,
        )
        .await?;
    let rivetos: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"RivetOS","aliases":["rivet"]}"#,
            StatusCode::CREATED,
        )
        .await?;
    let rail: SessionId = format!("claude-code:{RAIL}").parse()?;
    let first = conversation(
        &api,
        rail.as_str(),
        "claude-code",
        "Topic rail",
        Some("/work/rhd/crates/ui"),
    )
    .await?;
    let qwen = conversation(&api, "qwen-code:41", "qwen-code", "Quantised bench", None).await?;
    let counting = conversation(
        &api,
        &format!("claude-code:{COUNTING}"),
        "claude-code",
        "Counting with sleep delays",
        None,
    )
    .await?;
    let sandbox = conversation(
        &api,
        "grokbot-run-sand-subagent-7",
        "grokbot",
        "Routine",
        None,
    )
    .await?;
    let desktop_id: TopicId = desktop.id.parse()?;
    let rivetos_id: TopicId = rivetos.id.parse()?;
    let plans = file_all(&api, [first, qwen, counting, sandbox]).await?;
    assert_eq!(
        plans,
        [
            FilingPlan::File {
                target: Target::Session(rail),
                topic: desktop_id,
                rule: Rule::Folder
            },
            FilingPlan::File {
                target: Target::Conversation(qwen),
                topic: rivetos_id,
                rule: Rule::Model
            },
            FilingPlan::Refused {
                name: TopicName::new("Counting with sleep delays")?,
                refusal: Refusal::Title
            },
            FilingPlan::Never,
        ]
    );
    rows_and_corrections(&api, &desktop, qwen).await
}

async fn file_all(api: &Api, conversations: [Uuid; 4]) -> TestResult<Vec<FilingPlan>> {
    let provider = FakeProvider::new(vec![
        reply(r#"{"file": "T2"}"#),
        reply(r#"{"suggest": "Counting with sleep delays", "under": null}"#),
    ]);
    let topics = SessionTopics {
        store: api.store.clone(),
        filing: api.store.clone(),
        lineage: std::sync::Arc::new(capture::listing::StoreSessionLister::new(
            capture::listing::StoreRoots::default(),
        )),
    };
    let cancel = CancellationToken::new();
    let env = FilingEnv {
        topics: &topics,
        provider: &provider,
        model: "filer",
        now: Timestamp::now(),
        cancel: &cancel,
    };
    let mut plans = Vec::new();
    for conversation in conversations {
        plans.push(workers::handle_file(&env, conversation).await?);
    }
    Ok(plans)
}

async fn rows_and_corrections(api: &Api, desktop: &TopicRecord, qwen: Uuid) -> TestResult {
    let filed: TopicSessionList = api
        .json(
            "GET",
            &format!("/api/topics/{}/sessions", desktop.id),
            "",
            StatusCode::OK,
        )
        .await?;
    assert_eq!(
        filed
            .sessions
            .iter()
            .map(|row| (row.title.as_deref(), row.placement.by))
            .collect::<Vec<_>>(),
        [(Some("Topic rail"), PlacedBy::Categorizer)]
    );
    let moved: ConversationPlacementReply = api
        .json(
            "PUT",
            &format!("/api/conversations/{qwen}/placement"),
            &format!(r#"{{"topicId":"{}"}}"#, desktop.id),
            StatusCode::OK,
        )
        .await?;
    assert_eq!(moved.placement.map(|placed| placed.by), Some(PlacedBy::You));
    let listed: TopicSessionList = api
        .json(
            "GET",
            &format!("/api/topics/{}/sessions", desktop.id),
            "",
            StatusCode::OK,
        )
        .await?;
    assert_eq!(
        listed
            .conversations
            .iter()
            .map(|row| (row.channel.as_str(), row.title.as_deref()))
            .collect::<Vec<_>>(),
        [("qwen-code", Some("Quantised bench"))]
    );
    let corrections = api.store.corrections(8).await?;
    assert_eq!(
        corrections
            .iter()
            .map(|example| (
                example.from.as_str(),
                example.to.as_ref().map(TopicName::as_str)
            ))
            .collect::<Vec<_>>(),
        [("RivetOS", Some("Desktop App"))]
    );
    assert!(api.store.corrected(&Target::Conversation(qwen)).await?);
    Ok(())
}

fn reply(text: &str) -> Script {
    Script::Chat(LlmResponse {
        r#type: LlmResponseKind::Text,
        content: Some(text.to_owned()),
        tool_calls: None,
        usage: None,
        finish_reason: None,
    })
}

async fn conversation(
    api: &Api,
    key: &str,
    channel: &str,
    title: &str,
    cwd: Option<&str>,
) -> TestResult<Uuid> {
    let settings = match cwd {
        Some(cwd) => Some(JsonObject::from_serialize(&Settings { cwd })?),
        None => None,
    };
    let captured = MemoryStore::capture(
        api.store.as_ref(),
        kernel::memory::CaptureBatchRequest {
            session_key: key.to_owned(),
            agent: "rivet".to_owned(),
            channel: Some(channel.to_owned()),
            title: Some(title.to_owned()),
            settings,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![kernel::memory::CaptureMessage {
                event_id: format!("{key}-1"),
                role: Role::User,
                content: title.to_owned(),
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: None,
                created_at: None,
            }],
        },
        &testkit::owner()?,
        None,
    )
    .await?;
    let id = Uuid::parse_str(&captured.conversation_id)?;
    api.database()?.execute(
        "INSERT INTO ros_summaries (id, conversation_id, content, kind, created_at)
         VALUES (?1, ?2, ?3, 'leaf', '2026-10-29T12:00:00.000Z')",
        (
            Uuid::new_v4().to_string(),
            id.to_string(),
            format!("Worked on {title}."),
        ),
    )?;
    Ok(id)
}

#[derive(serde::Serialize)]
struct Settings<'a> {
    cwd: &'a str,
}

#[tokio::test]
async fn a_conversation_captured_here_names_this_node_and_its_summaries() -> TestResult {
    let api = Api::new(true)?;
    let bench: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"Bench"}"#,
            StatusCode::CREATED,
        )
        .await?;
    let id = captured_here(&api, "qwen-code:51").await?;
    api.database()?.execute(
        "INSERT INTO ros_summaries (id, conversation_id, content, kind, created_at)
         VALUES ('00000000-0000-4000-8000-000000000052', ?1, 'Benched it.', 'leaf',
                 '2026-10-29T12:00:00.000Z')",
        [id.as_str()],
    )?;
    let _: ConversationPlacementReply = api
        .json(
            "PUT",
            &format!("/api/conversations/{id}/placement"),
            &format!(r#"{{"topicId":"{}"}}"#, bench.id),
            StatusCode::OK,
        )
        .await?;
    let listed: TopicSessionList = api
        .json(
            "GET",
            &format!("/api/topics/{}/sessions", bench.id),
            "",
            StatusCode::OK,
        )
        .await?;
    assert_eq!(
        listed
            .conversations
            .iter()
            .map(|row| row.node.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("node-a")]
    );
    let dag: MemorySummaryDagResponse = api
        .json(
            "GET",
            &format!("/api/memory/conversations/{id}/summaries"),
            "",
            StatusCode::OK,
        )
        .await?;
    assert_eq!(dag.session_id, "qwen-code:51");
    assert_eq!(
        dag.summaries
            .iter()
            .map(|node| node.content.as_str())
            .collect::<Vec<_>>(),
        vec!["Benched it."]
    );
    Ok(())
}

async fn captured_here(api: &Api, key: &str) -> TestResult<String> {
    let batch = CaptureBatchRequest {
        session_key: key.to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("qwen-code".to_owned()),
        title: Some("Quantised bench".to_owned()),
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![CaptureMessage {
            event_id: format!("{key}-1"),
            role: Role::User,
            content: "Bench the quantised model.".to_owned(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: None,
        }],
    };
    let (status, _) = api
        .call("POST", "/api/capture", &serde_json::to_string(&batch)?)
        .await?;
    assert_eq!(status, StatusCode::OK);
    Ok(api.database()?.query_row(
        "SELECT id FROM ros_conversations WHERE session_key = ?1",
        [key],
        |row| row.get(0),
    )?)
}
