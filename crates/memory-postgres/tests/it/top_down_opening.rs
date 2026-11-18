use jiff::Timestamp;
use kernel::cleanup::CleanupSource as _;
use kernel::filing::FilingStore as _;
use kernel::memory::SummaryStore as _;
use kernel::owner::Owner;
use kernel::tenancy::{Scope, TenancyStore as _, Visibility};
use memory_postgres::{CaptureOptions, PostgresMemory, PostgresTenancy};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;
use wire::{
    JsonObject,
    session_id::{SessionId, SessionKey},
};

use crate::{
    common::probe,
    visibility::{batch, household, seed, viewer},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const SESSION: &str = "claude-code:00000000-0000-4000-8000-0000000000c1";
const FIRST: &str = "Repair the recovery callback";
const LATER: &str = "Inspect the missing receipts";

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_opening_uses_transcript_order_and_a_shared_reader_cannot_write_it(
    pool: PgPool,
) -> TestResult {
    seed(&pool).await?;
    let scoped = probe(&pool).await?;
    let memory = PostgresMemory::new(scoped.clone());
    let alice = viewer("alice")?;
    let owner = Owner::new("alice".to_owned())?;
    let scope = Scope::Viewer(alice.clone());
    let options = CaptureOptions::owned_by(&owner);
    let later = positioned(7, LATER)?;
    let captured = scope
        .clone()
        .run(memory.capture_batch(&later, &options))
        .await?;
    let id = Uuid::parse_str(&captured.conversation_id)?;
    let previous = scope
        .clone()
        .run(memory.opening_title(id))
        .await?
        .expect("opening");
    assert_eq!(previous.content, LATER);

    let early = positioned(2, FIRST)?;
    let moved = scope
        .clone()
        .run(memory.capture_batch(&early, &options))
        .await?;
    assert_eq!(moved.conversation_id, captured.conversation_id);
    assert_eq!(
        scope
            .clone()
            .run(memory.capture_batch(&later, &options))
            .await?
            .inserted,
        0
    );
    assert!(
        !scope
            .clone()
            .run(memory.apply_opening_title(&previous, Some("Stale title")))
            .await?
    );
    let projected: (String, bool, bool) = sqlx::query_as(
        "SELECT content, filing_armed, title_armed FROM rh_summary_openings WHERE conversation_id = $1",
    ).bind(id).fetch_one(&pool).await?;
    assert_eq!(projected, (FIRST.to_owned(), true, true));

    let bob = Scope::Viewer(viewer("bob")?);
    assert!(bob.clone().run(memory.subject(id)).await?.is_none());
    let tenancy = PostgresTenancy::new(scoped.clone());
    let session = SessionId::try_from(SESSION.to_owned())?;
    scope
        .run(tenancy.mark_session(
            &alice,
            &SessionKey::from(&session),
            Some(&Visibility::Space(household()?)),
            Timestamp::now(),
        ))
        .await?;
    let visible = bob
        .clone()
        .run(memory.subject(id))
        .await?
        .expect("visible opening");
    assert_eq!(visible.summary.as_deref(), Some(FIRST));
    let changed = bob.run(async {
        sqlx::query("UPDATE rh_summary_openings SET content = 'Should be rejected' WHERE conversation_id = $1")
            .bind(id).execute(&scoped).await
    }).await?;
    assert_eq!(changed.rows_affected(), 0);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn cleanup_reads_the_same_opening_while_the_conversation_has_no_leaf(
    pool: PgPool,
) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let owner = Owner::new("alice".to_owned())?;
    let captured = memory
        .capture_batch(&positioned(2, FIRST)?, &CaptureOptions::owned_by(&owner))
        .await?;
    let id = Uuid::parse_str(&captured.conversation_id)?;
    assert_eq!(
        memory.conversation(id).await?.expect("conversation").leaves,
        []
    );
    let readonly = PgPoolOptions::new()
        .connect_with(
            pool.connect_options()
                .as_ref()
                .clone()
                .options([("default_transaction_read_only", "on")]),
        )
        .await?;
    let snapshot = PostgresMemory::new(readonly).snapshot().await?;
    let row = snapshot
        .conversations
        .iter()
        .find(|row| row.head.id == id)
        .expect("projected row");
    assert_eq!(row.opening.as_deref(), Some(FIRST));
    assert_eq!(row.summary.as_deref(), Some(FIRST));
    Ok(())
}

fn positioned(
    line: u64,
    text: &str,
) -> Result<kernel::memory::CaptureBatchRequest, serde_json::Error> {
    let mut capture = batch(SESSION, text);
    let mut position = JsonObject::default();
    position.insert("session_jsonl_line", line)?;
    position.insert("session_jsonl_path", "/invented/session.jsonl")?;
    for message in &mut capture.messages {
        message.metadata = Some(position.clone());
    }
    Ok(capture)
}
