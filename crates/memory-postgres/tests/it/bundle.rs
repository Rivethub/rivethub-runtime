use jiff::Timestamp;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::SessionDeletion as _;
use kernel::owner::Owner;
use memory_postgres::CaptureOptions;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use uuid::Uuid;
use wire::JsonObject;
use wire::capture::BundleAuthor;
use wire::capture::BundleFilter;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn author(user: &str) -> BundleAuthor {
    BundleAuthor {
        user: user.to_owned(),
        node: "desk".to_owned(),
    }
}

fn at(text: &str) -> Result<Timestamp, jiff::Error> {
    wire::time::parse_rfc3339(text)
}

fn object(text: &str) -> Result<JsonObject, serde_json::Error> {
    serde_json::from_str(text)
}

fn said(event_id: &str, content: &str, created_at: Timestamp) -> CaptureMessage {
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
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let owner = Owner::new(owner.to_owned())?;
    let result = store
        .capture_batch(batch, &CaptureOptions::owned_by(&owner))
        .await?;
    Ok(Uuid::parse_str(&result.conversation_id)?)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn capture_rejects_placement_past_the_timestamp_ceiling(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let owner = CaptureOptions::owned_by(&Owner::new("dana".to_owned())?);
    let last = at("9999-12-30T22:00:00.999999Z")?;
    let first = at("9999-12-30T22:00:00.999998Z")?;
    let tied = batch(
        "claude-code:tied-ceiling",
        vec![said("one", "one", last), said("two", "two", last)],
    );
    assert!(matches!(
        store.capture_batch(&tied, &owner).await,
        Err(memory_postgres::error::Error::Operation {
            operation: "message time placement",
            ..
        })
    ));
    let key = "claude-code:undated-ceiling";
    let id = captured(
        &store,
        "dana",
        &batch(key, vec![said("seed", "seed", first)]),
    )
    .await?;
    let undated = |event_id| CaptureMessage {
        created_at: None,
        ..said(event_id, event_id, first)
    };
    let overflow = batch(key, vec![undated("one"), undated("two")]);
    assert!(matches!(
        store.capture_batch(&overflow, &owner).await,
        Err(memory_postgres::error::Error::Operation {
            operation: "message insert",
            ..
        })
    ));
    let count: i64 =
        sqlx::query_scalar("select count(*) from rh_messages where conversation_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(count, 1);
    let _last = captured(
        &store,
        "dana",
        &batch(key, vec![said("last", "last", last)]),
    )
    .await?;
    let beyond = batch(key, vec![undated("beyond")]);
    assert!(matches!(
        store.capture_batch(&beyond, &owner).await,
        Err(memory_postgres::error::Error::Operation {
            operation: "capture start time",
            ..
        })
    ));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn export_selects_only_named_sessions_in_message_order(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let mut tool = said("evt-tool", "[tool call] Read", at("2026-10-14T09:05:00Z")?);
    tool.role = Role::Tool;
    tool.tool_name = Some("Read".to_owned());
    tool.tool_args = Some(object(r#"{"file_path":"/work/a.rs"}"#)?);
    tool.tool_result = Some("fn main() {}\n\twith a tab and \"quotes\"".to_owned());
    tool.metadata = Some(object(r#"{"uuid":"u-1"}"#)?);
    let mut first = batch(
        "claude-code:first",
        vec![
            said("evt-late", "said last", at("2026-10-14T09:10:00Z")?),
            said("evt-early", "said first", at("2026-10-14T09:00:00Z")?),
            tool.clone(),
        ],
    );
    first.settings = Some(object(r#"{"model":"opus"}"#)?);
    let first_id = captured(&store, "dana", &first).await?;
    let skipped = batch(
        "claude-code:skipped",
        vec![said(
            "evt-skip",
            "not exported",
            at("2026-10-14T09:00:00Z")?,
        )],
    );
    captured(&store, "dana", &skipped).await?;
    let third = batch(
        "codex:third",
        vec![said("evt-third", "third", at("2026-10-14T10:00:00Z")?)],
    );
    let third_id = captured(&store, "dana", &third).await?;

    let sessions = store
        .bundled_sessions(
            &author("dana"),
            Some(&[third_id, first_id]),
            &BundleFilter::default(),
        )
        .await?;

    let mut ids: Vec<_> = sessions
        .iter()
        .map(|session| session.origin.conversation_id.clone())
        .collect();
    ids.sort();
    let mut wanted = vec![first_id.to_string(), third_id.to_string()];
    wanted.sort();
    assert_eq!(ids, wanted);
    let exported = sessions
        .iter()
        .find(|session| session.origin.conversation_id == first_id.to_string())
        .ok_or("first session")?;
    assert_eq!(exported.origin.author, "dana");
    assert_eq!(exported.origin.session_key, "claude-code:first");
    assert_eq!(exported.batch.settings, first.settings);
    assert_eq!(
        exported.batch.title.as_deref(),
        Some("title of claude-code:first")
    );
    assert_eq!(
        exported
            .batch
            .messages
            .iter()
            .map(|message| message.event_id.as_str())
            .collect::<Vec<_>>(),
        ["evt-early", "evt-tool", "evt-late"]
    );
    let read = exported
        .batch
        .messages
        .iter()
        .find(|message| message.event_id == "evt-tool")
        .ok_or("tool message")?;
    assert_eq!(read.tool_name, tool.tool_name);
    assert_eq!(read.tool_args, tool.tool_args);
    assert_eq!(read.tool_result, tool.tool_result);
    assert_eq!(read.metadata, tool.metadata);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn export_never_returns_another_owners_session(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let danas = captured(
        &store,
        "dana",
        &batch(
            "claude-code:dana",
            vec![said("evt-p", "mine", at("2026-10-14T09:00:00Z")?)],
        ),
    )
    .await?;
    let iness = captured(
        &store,
        "ines",
        &batch(
            "claude-code:ines",
            vec![said("evt-a", "not dana's", at("2026-10-14T09:00:00Z")?)],
        ),
    )
    .await?;

    let named = store
        .bundled_sessions(
            &author("dana"),
            Some(&[iness, danas]),
            &BundleFilter::default(),
        )
        .await?;
    let everything = store
        .bundled_sessions(&author("dana"), None, &BundleFilter::default())
        .await?;

    for sessions in [&named, &everything] {
        let ids: Vec<_> = sessions
            .iter()
            .map(|session| session.origin.conversation_id.clone())
            .collect();
        assert_eq!(ids, [danas.to_string()]);
    }
    assert!(!serde_json::to_string(&everything)?.contains("not dana's"));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_trashed_session_is_exported_only_when_named(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let kept = captured(
        &store,
        "dana",
        &batch(
            "claude-code:kept",
            vec![said("evt-k", "kept", at("2026-10-14T09:00:00Z")?)],
        ),
    )
    .await?;
    let trashed = captured(
        &store,
        "dana",
        &batch(
            "claude-code:trashed",
            vec![said("evt-t", "trashed", at("2026-10-14T09:01:00Z")?)],
        ),
    )
    .await?;
    store
        .trash_session("claude-code:trashed", None, at("2026-10-15T00:00:00Z")?)
        .await?;

    let everything = store
        .bundled_sessions(&author("dana"), None, &BundleFilter::default())
        .await?;
    let named = store
        .bundled_sessions(&author("dana"), Some(&[trashed]), &BundleFilter::default())
        .await?;

    let ids = |sessions: &[wire::capture::BundledSession]| -> Vec<String> {
        sessions
            .iter()
            .map(|session| session.origin.conversation_id.clone())
            .collect()
    };
    assert_eq!(ids(&everything), [kept.to_string()]);
    assert_eq!(ids(&named), [trashed.to_string()]);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn export_keeps_capture_order_among_messages_captured_without_a_timestamp(
    pool: PgPool,
) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let tied = at("2026-10-14T09:00:00Z")?;
    let steps: Vec<String> = (1..=8).map(|step| format!("step {step}")).collect();
    let messages = steps
        .iter()
        .map(|step| CaptureMessage {
            created_at: None,
            ..said(&format!("evt-{step}"), step, tied)
        })
        .collect();
    let id = captured(&store, "dana", &batch("claude-code:undated", messages)).await?;

    let sessions = store
        .bundled_sessions(&author("dana"), Some(&[id]), &BundleFilter::default())
        .await?;

    let session = sessions.first().ok_or("undated session")?;
    let exported: Vec<&str> = session
        .batch
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect();
    assert_eq!(exported, steps);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_batch_that_waits_on_the_session_lock_sorts_after_the_batch_holding_it(
    pool: PgPool,
) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let key = "claude-code:overlap";
    let undated = |content: &str| CaptureMessage {
        created_at: None,
        ..said(&format!("evt-{content}"), content, Timestamp::now())
    };
    let id = captured(&store, "dana", &batch(key, vec![undated("seed")])).await?;
    let warm = (pool.acquire().await?, pool.acquire().await?);
    drop(warm);

    let first: Vec<String> = (0..3).map(|n| format!("a {n}")).collect();
    let metadata = object("{}")?;
    let mut holder = memory_postgres::CaptureWriter::begin(&pool, key).await?;
    let second = tokio::spawn({
        let store = PostgresMemory::new(pool.clone());
        let request = batch(key, vec![undated("b")]);
        async move {
            captured(&store, "dana", &request)
                .await
                .map_err(|e| e.to_string())
        }
    });
    while sqlx::query_scalar::<_, i64>(
        "select count(*) from pg_locks where locktype = 'advisory' and not granted",
    )
    .fetch_one(&pool)
    .await?
        == 0
    {
        tokio::task::yield_now().await;
    }
    for (index, content) in first.iter().enumerate() {
        holder
            .insert_message(&memory_postgres::MessageRow {
                conversation_id: id,
                agent: "rivet",
                channel: "claude-code",
                role: Role::User,
                content,
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: &metadata,
                owner_user_id: "dana",
                created_at: None,
                batch_index: i64::try_from(index)? * 5_000_000,
            })
            .await?;
    }
    holder.commit().await?;
    second.await??;

    let sessions = store
        .bundled_sessions(&author("dana"), Some(&[id]), &BundleFilter::default())
        .await?;

    let session = sessions.first().ok_or("overlap session")?;
    let exported: Vec<&str> = session
        .batch
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect();
    let expected: Vec<&str> = std::iter::once("seed")
        .chain(first.iter().map(String::as_str))
        .chain(std::iter::once("b"))
        .collect();
    assert_eq!(exported.len(), expected.len());
    assert_eq!(
        exported.iter().position(|c| *c == "b"),
        Some(expected.len() - 1)
    );
    assert_eq!(exported, expected);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_rls_login_bundles_every_owner_only_in_the_maintenance_scope(
    pool: PgPool,
) -> TestResult {
    let seeded = PostgresMemory::new(pool.clone());
    let stamp = at("2026-03-01T10:00:00Z")?;
    let mine = captured(
        &seeded,
        "dana",
        &batch("claude-code:mine", vec![said("a", "a", stamp)]),
    )
    .await?;
    let theirs = captured(
        &seeded,
        "ben",
        &batch("claude-code:theirs", vec![said("b", "b", stamp)]),
    )
    .await?;
    let store = PostgresMemory::new(crate::common::probe(&pool).await?);
    let everyone = BundleFilter {
        owners: wire::capture::OwnerScope::All,
        ..BundleFilter::default()
    };
    let bens = BundleFilter {
        owners: wire::capture::OwnerScope::Listed(vec!["ben".to_owned()]),
        ..BundleFilter::default()
    };

    let unscoped = store
        .bundled_sessions(&author("dana"), None, &everyone)
        .await?;
    assert_eq!(unscoped.len(), 0);

    let all = kernel::tenancy::Scope::Maintenance
        .run(store.bundled_sessions(&author("dana"), None, &everyone))
        .await?;
    let mut found: Vec<String> = all
        .iter()
        .map(|session| session.origin.conversation_id.clone())
        .collect();
    found.sort();
    let mut expected = vec![mine.to_string(), theirs.to_string()];
    expected.sort();
    assert_eq!(found, expected);

    let only_ben = kernel::tenancy::Scope::Maintenance
        .run(store.bundled_sessions(&author("dana"), Some(&[theirs]), &bens))
        .await?;
    assert_eq!(only_ben.len(), 1);
    Ok(())
}
