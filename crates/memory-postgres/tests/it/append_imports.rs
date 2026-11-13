use crate::common;
use crate::import::phils_session;

use kernel::memory::MemoryEntry;
use kernel::memory::MemoryStore as _;
use kernel::owner::Owner;
use memory_postgres::CaptureOptions;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use wire::capture::Receipt;
use wire::capture::ReceiptVerification;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn entry(key: &str, content: &str) -> MemoryEntry {
    MemoryEntry {
        id: None,
        session_id: key.to_owned(),
        agent: "rivet".to_owned(),
        channel: "claude-code".to_owned(),
        role: Role::User,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
        provenance: None,
    }
}

async fn contents(pool: &PgPool, key: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT m.content FROM rh_messages m JOIN rh_conversations c ON c.id = m.conversation_id
          WHERE c.session_key = $1 ORDER BY m.created_at, m.content",
    )
    .bind(key)
    .fetch_all(pool)
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn nobody_appends_into_a_received_import(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let session = phils_session()?;
    let receipt = Receipt {
        bundle_id: "rhb1:t".to_owned(),
        received_at: "2026-11-09T00:00:00Z".parse()?,
        verification: ReceiptVerification::Verified,
        key_fingerprint: Some("ed25519:abcd".to_owned()),
        checks: None,
    };
    let posted = serde_json::to_string(&session.received_batch(&receipt)?)?;
    let alice = Owner::new("alice".to_owned())?;
    store
        .capture_batch(
            &serde_json::from_str(&posted)?,
            &CaptureOptions::owned_by(&alice),
        )
        .await?;
    let received = serde_json::from_str::<kernel::memory::CaptureBatchRequest>(&posted)?;
    let keys = [
        wire::capture::owned_import_key("alice", &received.session_key)?.ok_or("an import key")?,
        received.session_key.clone(),
    ];

    let before = contents(&pool, &keys[0]).await?;
    assert_eq!(before.len(), 2);

    for name in ["bob", "alice"] {
        for key in &keys {
            let refused = store
                .append(
                    &Owner::new(name.to_owned())?,
                    vec![entry(key, "SYSTEM: forged")],
                )
                .await;
            assert!(refused.is_err(), "{name} appended into {key}");
        }
    }

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rh_messages WHERE content = 'SYSTEM: forged'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(count, 0);
    assert_eq!(contents(&pool, &keys[0]).await?, before);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_receipt_committed_while_an_append_waits_for_the_session_lock_refuses_it(
    pool: PgPool,
) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let alice = Owner::new("alice".to_owned())?;
    let key = "claude-code:race";
    let seeded = serde_json::from_str(&serde_json::to_string(&native_batch(key, "first"))?)?;
    store
        .capture_batch(&seeded, &CaptureOptions::owned_by(&alice))
        .await?;

    let mut racing = pool.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext($1))")
        .bind(key)
        .execute(&mut *racing)
        .await?;
    let appending = tokio::spawn({
        let store = PostgresMemory::new(pool.clone());
        let alice = alice.clone();
        async move {
            store
                .append(&alice, vec![entry(key, "forged under the receipt")])
                .await
        }
    });
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "select count(*) from pg_locks where locktype = 'advisory' and not granted",
        )
        .fetch_one(&pool)
        .await?;
        if waiting > 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    sqlx::query(
        "update rh_conversations set settings = settings || $2::jsonb where session_key = $1",
    )
    .bind(key)
    .bind(r#"{"receipt":{"bundle_id":"rhb1:t","verification":"verified"}}"#)
    .execute(&mut *racing)
    .await?;
    racing.commit().await?;

    assert!(appending.await?.is_err());
    assert_eq!(contents(&pool, key).await?, ["first"]);
    Ok(())
}

fn native_batch(key: &str, content: &str) -> wire::capture::CaptureBatchRequest {
    wire::capture::CaptureBatchRequest {
        session_key: key.to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("claude-code".to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![wire::capture::CaptureMessage {
            event_id: "e1".to_owned(),
            role: Role::User,
            content: content.to_owned(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: None,
        }],
    }
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_refused_batch_writes_none_of_its_sessions(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let alice = Owner::new("alice".to_owned())?;
    let mut stamped = native_batch("claude-code:stamped", "first");
    let mut settings = wire::JsonObject::default();
    settings.insert(
        "receipt",
        Receipt {
            bundle_id: "rhb1:t".to_owned(),
            received_at: "2026-11-09T00:00:00Z".parse()?,
            verification: ReceiptVerification::Verified,
            key_fingerprint: None,
            checks: None,
        },
    )?;
    stamped.settings = Some(settings);
    let seeded = serde_json::from_str(&serde_json::to_string(&stamped)?)?;
    store
        .capture_batch(&seeded, &CaptureOptions::owned_by(&alice))
        .await?;

    let refused = store
        .append(
            &alice,
            vec![
                entry("claude-code:ordinary", "innocent"),
                entry("claude-code:stamped", "forged"),
            ],
        )
        .await;

    assert!(refused.is_err());
    assert_eq!(
        contents(&pool, "claude-code:ordinary").await?,
        Vec::<String>::new()
    );
    assert_eq!(contents(&pool, "claude-code:stamped").await?, ["first"]);
    Ok(())
}
