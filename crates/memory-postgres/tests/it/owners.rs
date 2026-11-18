use jiff::Timestamp;
use kernel::filing::ConversationPlacement;
use kernel::filing::FilingStore as _;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::NewSummary;
use kernel::memory::SummaryKind;
use kernel::memory::SummarySource;
use kernel::memory::SummaryStore as _;
use kernel::owner::Owner;
use kernel::topics::NewTopic;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use kernel::topics::TopicStore as _;
use memory_postgres::CaptureOptions;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use uuid::Uuid;
use wire::message::Role;
use wire::session_id::SessionId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CAPTURED: &str = "claude-code:00000000-0000-4000-8000-0000000000a1";
const LIVE: &str = "claude-code:00000000-0000-4000-8000-0000000000a2";

fn batch(event: &str) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: CAPTURED.to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("cli".to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![CaptureMessage {
            event_id: event.to_owned(),
            role: Role::User,
            content: format!("message {event}"),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: None,
        }],
    }
}

async fn owner_of(pool: &PgPool, sql: &'static str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(sql).fetch_all(pool).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn rows_derived_from_a_conversation_take_its_owner(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let alice = Owner::new("alice".to_owned())?;
    let bob = Owner::new("bob".to_owned())?;
    store
        .capture_batch(&batch("first"), &CaptureOptions::owned_by(&alice))
        .await?;
    store
        .capture_batch(&batch("second"), &CaptureOptions::owned_by(&bob))
        .await?;
    let (conversation, message): (Uuid, Uuid) = sqlx::query_as(
        "SELECT c.id, m.id FROM rh_conversations c JOIN rh_messages m ON m.conversation_id = c.id
          ORDER BY m.created_at LIMIT 1",
    )
    .fetch_one(&pool)
    .await?;
    let now = Timestamp::now();
    store
        .write_summary(NewSummary {
            id: Uuid::new_v4(),
            conversation_id: conversation,
            kind: SummaryKind::Leaf,
            depth: 0,
            content: "a leaf".to_owned(),
            message_count: 1,
            earliest_at: now,
            latest_at: now,
            model: "model".to_owned(),
            pipeline_version: 1,
            created_at: now,
            sources: vec![SummarySource {
                message_id: message,
                ordinal: 0,
            }],
            child_ids: Vec::new(),
        })
        .await?;
    let topic = store
        .create(Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Work,
                name: TopicName::new("Desk")?,
                aliases: Vec::new(),
                parent: None,
                directory: None,
                owner: Some(Owner::new("carol".to_owned())?),
            },
            now,
        )?)
        .await?;
    for session in [CAPTURED, LIVE] {
        store
            .place(Placement {
                session: SessionId::try_from(session.to_owned())?,
                topic: topic.id,
                by: PlacementSource::You,
                at: now,
                owner: Some(bob.clone()),
            })
            .await?;
    }
    store
        .place_conversation(ConversationPlacement {
            conversation,
            topic: topic.id,
            by: PlacementSource::Categorizer,
            at: now,
        })
        .await?;

    let alice_only = vec!["alice".to_owned()];
    assert_eq!(
        owner_of(&pool, "SELECT DISTINCT owner_user_id FROM rh_conversations").await?,
        alice_only
    );
    assert_eq!(
        owner_of(&pool, "SELECT DISTINCT owner_user_id FROM rh_messages").await?,
        alice_only,
        "a later caller's messages belong to the conversation's owner"
    );
    assert_eq!(
        owner_of(&pool, "SELECT DISTINCT owner_user_id FROM rh_summaries").await?,
        alice_only
    );
    assert_eq!(
        owner_of(
            &pool,
            "SELECT owner_user_id FROM rh_conversation_placements"
        )
        .await?,
        alice_only
    );
    assert_eq!(
        owner_of(
            &pool,
            "SELECT owner_user_id FROM rh_session_placements ORDER BY session_id"
        )
        .await?,
        ["alice", "bob"],
        "a captured session's placement is its owner's; a live one's is whoever placed it"
    );
    assert_eq!(topic.owner, Some(Owner::new("carol".to_owned())?));
    Ok(())
}
