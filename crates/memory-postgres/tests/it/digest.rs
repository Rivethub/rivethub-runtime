use crate::common;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::SessionDeletion as _;
use kernel::memory::SessionDigest;
use kernel::owner::Owner;
use memory_postgres::CaptureOptions;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use uuid::Uuid;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn at(text: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    Ok(DateTime::parse_from_rfc3339(text)?.with_timezone(&Utc))
}

fn said(event_id: &str, created_at: DateTime<Utc>) -> CaptureMessage {
    CaptureMessage {
        event_id: event_id.to_owned(),
        role: Role::User,
        content: format!("said {event_id}"),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(created_at),
    }
}

fn batch(session_key: &str, messages: Vec<CaptureMessage>) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: session_key.to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("claude-code".to_owned()),
        title: Some(format!("title of {session_key}")),
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages,
    }
}

async fn captured(
    store: &PostgresMemory,
    owner: &str,
    batch: &CaptureBatchRequest,
) -> Result<(Uuid, String), Box<dyn std::error::Error>> {
    let owner = Owner::new(owner.to_owned())?;
    let result = store
        .capture_batch(batch, &CaptureOptions::owned_by(&owner))
        .await?;
    let id = Uuid::parse_str(&result.conversation_id)?;
    let key: String = sqlx::query_scalar("select session_key from rh_conversations where id = $1")
        .bind(id)
        .fetch_one(store.pool())
        .await?;
    Ok((id, key))
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn digests_count_messages_and_dates_per_key(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let (long_id, long_key) = captured(
        &store,
        "phil",
        &batch(
            "claude-code:long",
            vec![
                said("evt-2", at("2026-10-14T09:30:00Z")?),
                said("evt-1", at("2026-10-14T09:00:00Z")?),
                said("evt-3", at("2026-10-15T08:00:00Z")?),
            ],
        ),
    )
    .await?;
    let mut imported = batch(
        "import:[\"laptop\",\"00000000-0000-4000-8000-000000000009\"]",
        vec![said("evt-i", at("2026-09-01T10:00:00Z")?)],
    );
    imported.settings = Some(serde_json::from_str(
        r#"{"origin":{"author":"ana","node":"laptop","conversation_id":"00000000-0000-4000-8000-000000000009","session_key":"codex:shared","agent":"codex","created_at":"2026-09-01T10:00:00Z","updated_at":"2026-09-01T10:00:00Z"}}"#,
    )?);
    let (imported_id, imported_key) = captured(&store, "phil", &imported).await?;
    let (_, trashed_key) = captured(
        &store,
        "phil",
        &batch(
            "claude-code:trashed",
            vec![said("evt-t", at("2026-10-14T09:00:00Z")?)],
        ),
    )
    .await?;
    store
        .trash_session("claude-code:trashed", None, at("2026-10-16T00:00:00Z")?)
        .await?;

    let digests = store
        .session_digests(
            &Owner::new("phil".to_owned())?,
            &[
                long_key.clone(),
                imported_key.clone(),
                trashed_key,
                "claude-code:never-captured".to_owned(),
            ],
        )
        .await?;

    let mut expected = vec![
        SessionDigest {
            key: long_key.clone(),
            conversation_id: long_id,
            title: Some("title of claude-code:long".to_owned()),
            agent: "rivet".to_owned(),
            first: at("2026-10-14T09:00:00Z")?,
            last: at("2026-10-15T08:00:00Z")?,
            messages: 3,
            imported_from: None,
        },
        SessionDigest {
            key: imported_key.clone(),
            conversation_id: imported_id,
            title: Some(format!("title of {}", imported.session_key)),
            agent: "rivet".to_owned(),
            first: at("2026-09-01T10:00:00Z")?,
            last: at("2026-09-01T10:00:00Z")?,
            messages: 1,
            imported_from: Some("ana".to_owned()),
        },
    ];
    expected.sort_by(|left, right| left.key.cmp(&right.key));
    assert_eq!(digests, expected);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn digests_never_return_another_owner(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let (_, phils) = captured(
        &store,
        "phil",
        &batch(
            "claude-code:phil",
            vec![said("evt-p", at("2026-10-14T09:00:00Z")?)],
        ),
    )
    .await?;
    let (_, alexs) = captured(
        &store,
        "alex",
        &batch(
            "claude-code:alex",
            vec![said("evt-a", at("2026-10-14T09:00:00Z")?)],
        ),
    )
    .await?;

    let digests = store
        .session_digests(&Owner::new("phil".to_owned())?, &[alexs, phils.clone()])
        .await?;

    let keys: Vec<&str> = digests.iter().map(|digest| digest.key.as_str()).collect();
    assert_eq!(keys, [phils.as_str()]);
    Ok(())
}
