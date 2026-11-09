use crate::common;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryStore as _;
use kernel::owner::Owner;
use memory_postgres::CaptureOptions;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use uuid::Uuid;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::SessionOrigin;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn at(text: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    Ok(DateTime::parse_from_rfc3339(text)?.with_timezone(&Utc))
}

fn said(event_id: &str, content: &str, created_at: DateTime<Utc>) -> CaptureMessage {
    CaptureMessage {
        event_id: event_id.to_owned(),
        role: Role::User,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(created_at),
    }
}

fn phils_session() -> Result<BundledSession, Box<dyn std::error::Error>> {
    Ok(BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: "desk".to_owned(),
            conversation_id: "7b0e7d1c-0000-4000-8000-000000000001".to_owned(),
            session_key: "claude-code:shared".to_owned(),
            agent: "rivet".to_owned(),
            created_at: Some(at("2026-10-14T09:00:00Z")?),
            updated_at: Some(at("2026-10-14T11:30:00Z")?),
        },
        batch: CaptureBatchRequest {
            session_key: "claude-code:shared".to_owned(),
            agent: "rivet".to_owned(),
            channel: Some("claude-code".to_owned()),
            title: Some("sqlite bundle migration".to_owned()),
            settings: Some(serde_json::from_str(r#"{"model":"opus"}"#)?),
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![
                said(
                    "evt-1",
                    "move the bundle reader",
                    at("2026-10-14T09:00:00Z")?,
                ),
                said("evt-2", "digest matches now", at("2026-10-14T11:30:00Z")?),
            ],
        },
    })
}

async fn import(
    store: &PostgresMemory,
    receiver: &str,
    session: &BundledSession,
) -> Result<kernel::memory::CaptureResult, Box<dyn std::error::Error>> {
    let owner = Owner::new(receiver.to_owned())?;
    let posted = serde_json::to_string(&session.import_batch()?)?;
    Ok(store
        .capture_batch(
            &serde_json::from_str(&posted)?,
            &CaptureOptions::owned_by(&owner),
        )
        .await?)
}

#[derive(sqlx::FromRow)]
struct Stored {
    owner_user_id: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    author: Option<String>,
    origin_conversation: Option<String>,
    model: Option<String>,
}

async fn stored(pool: &PgPool, id: Uuid) -> Result<Stored, sqlx::Error> {
    sqlx::query_as::<_, Stored>(
        "select owner_user_id, created_at, updated_at, \
         settings -> 'origin' ->> 'author' as author, \
         settings -> 'origin' ->> 'conversation_id' as origin_conversation, \
         settings ->> 'model' as model \
         from rh_conversations where id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn imported_conversation_keeps_origin_dates_and_receiver_owner(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let session = phils_session()?;

    let result = import(&store, "alex", &session).await?;

    let id = Uuid::parse_str(&result.conversation_id)?;
    let row = stored(&pool, id).await?;
    assert_eq!(row.owner_user_id, "alex");
    assert_eq!(Some(row.created_at), session.origin.created_at);
    assert_eq!(Some(row.updated_at), session.origin.updated_at);
    assert_eq!(row.author.as_deref(), Some("phil"));
    assert_eq!(
        row.origin_conversation.as_deref(),
        Some(session.origin.conversation_id.as_str())
    );
    assert_eq!(row.model.as_deref(), Some("opus"));
    let owners: Vec<String> = sqlx::query_scalar(
        "select distinct owner_user_id from rh_messages where conversation_id = $1",
    )
    .bind(id)
    .fetch_all(&pool)
    .await?;
    assert_eq!(owners, ["alex"]);
    let message: String =
        sqlx::query_scalar("select id::text from rh_messages where conversation_id = $1 limit 1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    let entry = store.get_full(&message).await?.ok_or("imported message")?;
    let origin: SessionOrigin = serde_json::from_value(
        entry
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("origin").cloned())
            .ok_or("origin in metadata")?,
    )?;
    assert_eq!(origin, session.origin);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn reimport_adds_nothing(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let session = phils_session()?;
    let first = import(&store, "alex", &session).await?;

    let second = import(&store, "alex", &session).await?;

    assert_eq!((first.inserted, first.skipped), (2, 0));
    assert_eq!((second.inserted, second.skipped), (0, 2));
    assert_eq!(second.conversation_id, first.conversation_id);
    let id = Uuid::parse_str(&first.conversation_id)?;
    let row = stored(&pool, id).await?;
    assert_eq!(Some(row.updated_at), session.origin.updated_at);
    let messages: i64 =
        sqlx::query_scalar("select count(*) from rh_messages where conversation_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(messages, 2);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_import_never_moves_a_native_conversation_back_in_time(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let session = phils_session()?;
    let mut native = session.batch.clone();
    native.messages = vec![said("evt-native", "live work", at("2026-11-01T08:00:00Z")?)];
    let owner = Owner::new("phil".to_owned())?;
    let captured = store
        .capture_batch(
            &serde_json::from_str(&serde_json::to_string(&native)?)?,
            &CaptureOptions::owned_by(&owner),
        )
        .await?;
    let id = Uuid::parse_str(&captured.conversation_id)?;
    let before = stored(&pool, id).await?;

    import(&store, "alex", &session).await?;

    let after = stored(&pool, id).await?;
    assert_eq!(after.owner_user_id, "phil");
    assert_eq!(after.created_at, before.created_at);
    assert_eq!(after.updated_at, before.updated_at);
    Ok(())
}
