use std::{sync::Arc, time::Duration};

use chrono::Utc;
use kernel::filing::FilingStore as _;
use kernel::memory::{CaptureBatchRequest, CaptureMessage, SummaryStore as _};
use kernel::model::{FinishReason, LlmResponse, LlmResponseKind, ModelRequest};
use kernel::queue::{FixedJob, JobName, JobQueue as _, WorkerRole};
use kernel::topics::TopicStore as _;
use memory_sqlite::SqliteStore;
use testkit::fakes::{FakeProvider, Script};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::message::{Message, MessageContent, Role};
use workers::{
    EmbedSettings, FILING_SYSTEM_PROMPT, ProcessRole, RoleModel, Runner, RunnerConfig,
    SessionTopics, SummarySettings, TITLE_SYSTEM_PROMPT, WorkerDeps,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const REQUEST: &str = "Repair the deployment callback. The client loses its receipt.";

#[tokio::test]
async fn active_capture_titles_and_files_before_the_first_leaf_and_recovers_lost_jobs() -> TestResult
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("opening.sqlite");
    migrate::sqlite(&path)?;
    let store = Arc::new(SqliteStore::open(&path)?);
    let owner = testkit::owner()?;
    let now = Utc::now();
    let topic = store
        .create(testkit::topics::new_topic("Release planning", &[], None)?)
        .await?;
    let session = store
        .capture(batch("cli:receipt", None), &owner, None, now)
        .await?;
    let id = Uuid::parse_str(&session.conversation_id)?;
    let manually_named = store
        .capture(
            batch("cli:manual", Some("Session 123: manual…")),
            &owner,
            None,
            now,
        )
        .await?;
    let manual = Uuid::parse_str(&manually_named.conversation_id)?;
    let initial = store.conversation(id).await?.expect("conversation");
    assert_eq!(initial.leaves, []);

    lose_opening_jobs(&store, id).await?;
    let script = scripted();
    let sweep = WorkerRole::Summarizer
        .crontab(false)
        .into_iter()
        .find(|entry| entry.task == JobName::Fixed(FixedJob::EnqueueOpenings))
        .expect("opening recovery schedule");
    store.enqueue(sweep.job()).await?;
    run_role(&store, &script, ProcessRole::Summarizer).await?;
    run_role(&store, &script, ProcessRole::Categorizer).await?;

    let window = store.conversation(id).await?.expect("active session");
    assert_eq!(window.leaves, []);
    assert_eq!(
        window.conversation.title.as_deref(),
        Some("Plan blue-green AcmeKit deploys")
    );
    let named = store.conversation(manual).await?.expect("manual session");
    assert_eq!(
        named.conversation.title.as_deref(),
        Some("Session 123: manual…")
    );
    let placement = store.conversation_placement(id).await?.expect("filed");
    assert_eq!(placement.topic, topic.id);
    assert_eq!(store.opening_filing(id).await?, Some(false));
    assert!(!store.opening_title(id).await?.expect("opening").armed);
    assert_eq!(store.undelivered_openings().await?, []);

    store.enqueue(sweep.job()).await?;
    run_role(&store, &script, ProcessRole::Summarizer).await?;
    run_role(&store, &script, ProcessRole::Categorizer).await?;
    assert_opening_calls(&script.chat_requests().await);
    Ok(())
}

async fn lose_opening_jobs(store: &SqliteStore, id: Uuid) -> TestResult {
    store.with_conn(move |conn| {
        Ok((|| {
            conn.execute("DELETE FROM ros_jobs WHERE task = 'title-conversation'", [])?;
            conn.execute("UPDATE ros_jobs SET state = 'running', locked_at = '2026-01-01T00:00:00.000Z' WHERE task = 'file-conversation'", [])?;
            let active: bool = conn.query_row("SELECT active FROM ros_conversations WHERE id = ?1",
                [id.to_string()], |row| row.get(0))?;
            assert!(active);
            conn.execute("UPDATE ros_conversations SET title = 'Session cli:receipt' WHERE id = ?1", [id.to_string()])?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        })())
    }).await?.map_err(|error| error.to_string())?;
    Ok(())
}

fn assert_opening_calls(calls: &[ModelRequest]) {
    let inputs = |system| {
        calls
            .iter()
            .filter(|call| call.messages.first().and_then(message_text) == Some(system))
            .filter_map(|call| call.messages.get(1).and_then(message_text))
            .collect::<Vec<_>>()
    };
    assert_eq!(inputs(TITLE_SYSTEM_PROMPT), [REQUEST]);
    let filings = inputs(FILING_SYSTEM_PROMPT);
    assert_eq!(filings.len(), 2, "one filing per conversation");
    assert!(filings.iter().all(|input| input.contains(REQUEST)));
    assert_eq!(calls.len(), 3);
}

fn message_text(message: &Message) -> Option<&str> {
    match &message.content {
        MessageContent::Text(text) => Some(text),
        MessageContent::Parts(_) => None,
    }
}

fn scripted() -> Arc<FakeProvider> {
    let chat = |system, content: &str| Script::ChatFor {
        system,
        response: LlmResponse {
            r#type: LlmResponseKind::Text,
            content: Some(content.to_owned()),
            tool_calls: None,
            finish_reason: Some(FinishReason::Stop),
            usage: None,
        },
    };
    Arc::new(FakeProvider::new(vec![
        chat(
            TITLE_SYSTEM_PROMPT,
            r#"{"title":"Plan blue-green AcmeKit deploys"}"#,
        ),
        chat(FILING_SYSTEM_PROMPT, r#"{"suggest":"Release planning"}"#),
        chat(FILING_SYSTEM_PROMPT, r#"{"suggest":"Release planning"}"#),
    ]))
}

async fn run_role(
    store: &Arc<SqliteStore>,
    script: &Arc<FakeProvider>,
    role: ProcessRole,
) -> TestResult {
    let deps = WorkerDeps {
        targets: store.clone(),
        summaries: store.clone(),
        summarizer: script.clone(),
        categorizer: RoleModel {
            provider: script.clone(),
            model: "fake-chat".to_owned(),
        },
        embedder: script.clone(),
        embed: EmbedSettings::standard(),
        summary: SummarySettings::standard("fake-chat")?,
        scopes: None,
    };
    let runner = Runner::new(store.clone(), RunnerConfig::new(role, 1)?, deps).with_session_topics(
        SessionTopics {
            store: store.clone(),
            filing: store.clone(),
            lineage: Arc::new(testkit::wiki_routes::NoParents),
        },
    );
    let mut idle = runner.idle();
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let stop = stop.clone();
        async move { runner.run(stop).await }
    });
    drop(tokio::time::timeout(Duration::from_secs(5), idle.wait_for(|idle| *idle)).await??);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), run).await???;
    Ok(())
}

fn batch(key: &str, title: Option<&str>) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: key.to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("cli".to_owned()),
        title: title.map(str::to_owned),
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![CaptureMessage {
            event_id: key.to_owned(),
            role: Role::User,
            content: REQUEST.to_owned(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: None,
        }],
    }
}
