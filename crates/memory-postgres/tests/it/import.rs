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
    let receiver_key = session.receiver_session_key()?;
    let origins = store
        .lookup_origins(vec![receiver_key.clone(), "claude-code:shared".to_owned()])
        .await?;
    let origin: SessionOrigin = origins
        .get(&receiver_key)
        .ok_or("origin of the imported session")?
        .deserialize()?;
    assert_eq!(origin, session.origin);
    assert_eq!(origins.len(), 1);
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

async fn capture_live(
    store: &PostgresMemory,
    owner: &str,
    batch: &CaptureBatchRequest,
) -> Result<kernel::memory::CaptureResult, Box<dyn std::error::Error>> {
    let owner = Owner::new(owner.to_owned())?;
    Ok(store
        .capture_batch(
            &serde_json::from_str(&serde_json::to_string(batch)?)?,
            &CaptureOptions::owned_by(&owner),
        )
        .await?)
}

fn alexs_native_session() -> Result<CaptureBatchRequest, Box<dyn std::error::Error>> {
    Ok(CaptureBatchRequest {
        session_key: "claude-code:shared".to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("claude-code".to_owned()),
        title: Some("alex native work".to_owned()),
        settings: Some(serde_json::from_str(r#"{"cwd":"/work/alex"}"#)?),
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![said("evt-native", "live work", at("2026-11-01T08:00:00Z")?)],
    })
}

#[derive(sqlx::FromRow, Debug, PartialEq)]
struct Conversation {
    session_key: String,
    owner_user_id: String,
    title: Option<String>,
    settings: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

async fn conversation(pool: &PgPool, id: &str) -> Result<Conversation, Box<dyn std::error::Error>> {
    Ok(sqlx::query_as::<_, Conversation>(
        "select session_key, owner_user_id, title, settings::text as settings, created_at, \
         updated_at from rh_conversations where id = $1",
    )
    .bind(Uuid::parse_str(id)?)
    .fetch_one(pool)
    .await?)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_import_lands_beside_a_native_conversation_with_the_same_session_key(
    pool: PgPool,
) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let native = capture_live(&store, "alex", &alexs_native_session()?).await?;
    let before = conversation(&pool, &native.conversation_id).await?;

    let imported = import(&store, "alex", &phils_session()?).await?;

    assert_ne!(imported.conversation_id, native.conversation_id);
    assert_eq!(imported.inserted, 2);
    assert_eq!(conversation(&pool, &native.conversation_id).await?, before);
    let native_messages: i64 =
        sqlx::query_scalar("select count(*) from rh_messages where conversation_id = $1")
            .bind(Uuid::parse_str(&native.conversation_id)?)
            .fetch_one(&pool)
            .await?;
    assert_eq!(native_messages, 1);
    let receiver = conversation(&pool, &imported.conversation_id).await?;
    assert_eq!(
        receiver.session_key,
        "import:desk:7b0e7d1c-0000-4000-8000-000000000001"
    );
    assert_eq!(receiver.title.as_deref(), Some("sqlite bundle migration"));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn live_capture_keeps_capture_time_whatever_dates_the_batch_carries(
    pool: PgPool,
) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let mut live = alexs_native_session()?;
    live.created_at = Some(at("2031-01-01T00:00:00Z")?);
    live.updated_at = Some(at("2031-01-01T00:00:00Z")?);
    let started = Utc::now();

    let first = capture_live(&store, "alex", &live).await?;
    live.messages = vec![said(
        "evt-native-2",
        "more live work",
        at("2026-11-01T09:00:00Z")?,
    )];
    capture_live(&store, "alex", &live).await?;

    let row = conversation(&pool, &first.conversation_id).await?;
    let finished = Utc::now();
    assert!(
        started <= row.created_at && row.created_at <= finished,
        "{row:?}"
    );
    assert!(
        row.created_at <= row.updated_at && row.updated_at <= finished,
        "{row:?}"
    );
    Ok(())
}

#[derive(serde::Deserialize, Debug)]
struct Settings {
    pinned: Option<bool>,
    model: Option<String>,
    origin: Option<SessionOrigin>,
}

async fn settings(pool: &PgPool, id: &str) -> Result<Settings, Box<dyn std::error::Error>> {
    Ok(serde_json::from_str(
        &conversation(pool, id).await?.settings,
    )?)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_reimport_keeps_settings_the_receiver_added(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let session = phils_session()?;
    let first = import(&store, "alex", &session).await?;
    sqlx::query(
        "update rh_conversations set settings = settings || '{\"pinned\":true}' where id = $1",
    )
    .bind(Uuid::parse_str(&first.conversation_id)?)
    .execute(&pool)
    .await?;

    let second = import(&store, "alex", &session).await?;

    assert_eq!(second.conversation_id, first.conversation_id);
    let kept = settings(&pool, &first.conversation_id).await?;
    assert_eq!(kept.pinned, Some(true));
    assert_eq!(kept.origin.as_ref(), Some(&session.origin));
    assert_eq!(kept.model.as_deref(), Some("opus"));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn only_a_bundle_as_new_or_newer_replaces_the_stored_origin(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let session = phils_session()?;
    let first = import(&store, "alex", &session).await?;
    let mut older = session.clone();
    older.origin.updated_at = Some(at("2026-10-14T10:00:00Z")?);
    older.batch.settings = Some(serde_json::from_str(r#"{"model":"sonnet"}"#)?);
    let mut newer = older.clone();
    newer.origin.updated_at = Some(at("2026-10-20T10:00:00Z")?);

    import(&store, "alex", &older).await?;
    let after_older = settings(&pool, &first.conversation_id).await?;
    import(&store, "alex", &newer).await?;
    let after_newer = settings(&pool, &first.conversation_id).await?;

    assert_eq!(after_older.origin.as_ref(), Some(&session.origin));
    assert_eq!(after_older.model.as_deref(), Some("opus"));
    assert_eq!(after_newer.origin.as_ref(), Some(&newer.origin));
    assert_eq!(after_newer.model.as_deref(), Some("sonnet"));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_message_tied_with_stored_messages_lands_after_all_of_them(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let tied = at("2026-10-14T10:00:00.000Z")?;
    let earlier = at("2026-10-14T09:59:59.000Z")?;
    let mut first = phils_session()?;
    first.batch.messages = vec![said("a", "a", tied), said("b", "b", tied)];
    let mut second = phils_session()?;
    second.batch.messages = vec![
        said("x", "x", earlier),
        said("c", "c", tied),
        said("d", "d", tied),
    ];

    let written = import(&store, "alex", &first).await?;
    import(&store, "alex", &second).await?;

    let stored: Vec<(String, DateTime<Utc>)> = sqlx::query_as(
        "select metadata->>'event_id', created_at from rh_messages \
         where conversation_id = $1 order by created_at, id",
    )
    .bind(Uuid::parse_str(&written.conversation_id)?)
    .fetch_all(&pool)
    .await?;
    let order: Vec<&str> = stored.iter().map(|(event, _)| event.as_str()).collect();
    assert_eq!(order, ["x", "a", "b", "c", "d"]);
    let times: Vec<DateTime<Utc>> = stored.iter().map(|(_, time)| *time).collect();
    let tick = chrono::TimeDelta::microseconds(1);
    assert_eq!(
        times,
        [earlier, tied, tied + tick, tied + tick * 2, tied + tick * 3]
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn messages_sharing_a_millisecond_export_in_bundle_order(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let tied = at("2026-10-14T09:00:00.123Z")?;
    let steps: Vec<String> = (1..=8).map(|step| format!("step-{step}")).collect();
    let mut whole = phils_session()?;
    whole.batch.messages = steps.iter().map(|step| said(step, step, tied)).collect();
    let mut split = whole.clone();
    split.batch.session_key = "claude-code:split".to_owned();
    split.origin.session_key = "claude-code:split".to_owned();
    split.origin.conversation_id = "7b0e7d1c-0000-4000-8000-000000000002".to_owned();
    let mut second = split.clone();
    second.batch.messages = split.batch.messages.split_off(4);

    import(&store, "alex", &whole).await?;
    import(&store, "alex", &split).await?;
    import(&store, "alex", &second).await?;

    let exported = store
        .bundled_sessions(
            &wire::capture::BundleAuthor {
                user: "alex".to_owned(),
                node: "team".to_owned(),
            },
            None,
        )
        .await?;
    assert_eq!(exported.len(), 2);
    for session in &exported {
        let order: Vec<&str> = session
            .batch
            .messages
            .iter()
            .map(|message| message.event_id.as_str())
            .collect();
        assert_eq!(order, steps, "{}", session.origin.session_key);
    }
    Ok(())
}
