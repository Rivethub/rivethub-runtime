use jiff::Timestamp;
use kernel::memory::settlement::SettleStatus;
use kernel::memory::{MemoryStore as _, SummaryStore as _};
use kernel::owner::Owner;
use kernel::tenancy::{Scope, Viewer};
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use testkit::settlement::{TestResult, batch, expire, message, position_fills, revision_lifecycle};
use uuid::Uuid;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn revisions_and_ownership(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(crate::common::probe(&pool).await?);
    let alice = Owner::new("alice".to_owned())?;
    let bob = Owner::new("bob".to_owned())?;
    let scope = Scope::Viewer(Viewer::new(alice.clone(), Vec::new()));
    scope
        .clone()
        .run(revision_lifecycle(&store, &alice))
        .await?;
    scope.clone().run(position_fills(&store, &alice)).await?;

    let id: Uuid = sqlx::query_scalar(
        "SELECT id FROM rh_conversations WHERE session_key = 'claude-code:settlement-fixture'",
    )
    .fetch_one(&pool)
    .await?;
    let bob_scope = Scope::Viewer(Viewer::new(bob.clone(), Vec::new()));
    assert!(
        bob_scope
            .clone()
            .run(store.summary_state(id))
            .await?
            .is_none()
    );
    assert!(!bob_scope.clone().run(store.summary_eligible(id, 4)).await?);
    assert!(
        bob_scope
            .clone()
            .run(store.mark_summary_settled(&id.to_string(), &bob))
            .await?
            .is_none()
    );
    assert!(
        bob_scope
            .run(store.mark_summary_settled(&id.to_string(), &alice))
            .await?
            .is_none()
    );

    sqlx::query("UPDATE rh_summary_state SET summarized_through = input_revision, failed_revision = input_revision WHERE conversation_id = $1")
        .bind(id).execute(&pool).await?;
    let result = scope
        .clone()
        .run(store.mark_summary_settled(&id.to_string(), &alice))
        .await?
        .ok_or("summarized")?;
    assert_eq!(result.status, SettleStatus::AlreadySummarized);
    assert!(!scope.clone().run(store.summary_eligible(id, 4)).await?);
    assert_eq!(
        scope
            .run(store.summary_state(id))
            .await?
            .ok_or("state")?
            .failed_revision,
        None
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn historical_and_live_expiry_across_pages(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let alice = Owner::new("alice".to_owned())?;
    let live: Uuid = store
        .capture(
            batch("cli:live", vec![message("live", "2026-01-02T01:00:00Z")?]),
            &alice,
            None,
        )
        .await?
        .conversation_id
        .parse()?;
    let historical: Uuid = store
        .capture(
            batch(
                "cli:past",
                vec![message("past", "2026-01-02T01:00:00.123456789Z")?],
            ),
            &alice,
            None,
        )
        .await?
        .conversation_id
        .parse()?;
    sqlx::query("DELETE FROM rh_summary_state WHERE conversation_id = $1")
        .bind(historical)
        .execute(&pool)
        .await?;
    sqlx::query("UPDATE rh_conversations SET updated_at = clock_timestamp() + interval '5 days' WHERE id = $1")
        .bind(historical).execute(&pool).await?;
    let beat: Uuid = store
        .capture(
            batch(
                "heartbeat:pulse",
                vec![message("pulse", "2026-01-02T01:00:00Z")?],
            ),
            &alice,
            None,
        )
        .await?
        .conversation_id
        .parse()?;
    store
        .mark_summary_settled(&beat.to_string(), &alice)
        .await?
        .ok_or("heartbeat mark")?;
    assert!(!store.summary_eligible(beat, 1).await?);
    assert!(store.summary_state(historical).await?.is_none());
    assert!(!store.summary_eligible(live, 1).await?);
    expire(&store, Timestamp::now(), 1).await?;
    testkit::settlement::expiry_boundary(&store, live).await?;
    assert!(store.summary_eligible(live, 1).await?);
    assert!(store.summary_eligible(historical, 0).await?);
    let state = store
        .summary_state(historical)
        .await?
        .ok_or("historical seed")?;
    assert!(state.historical_only);
    assert_eq!(
        state.last_input_at,
        "2026-01-02T01:00:00.123456Z".parse::<Timestamp>()?
    );
    let promoted = store
        .mark_summary_settled(&historical.to_string(), &alice)
        .await?
        .ok_or("manual historical mark")?;
    assert_eq!(promoted.settled_at, state.settled_at);
    assert!(
        !store
            .summary_state(historical)
            .await?
            .ok_or("manual historical state")?
            .historical_only
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn older_legacy_end_does_not_override_known_activity_and_late_input_clears_failure(
    pool: PgPool,
) -> TestResult {
    use kernel::memory::SessionDeletion as _;
    use testkit::settlement::observed;

    let store = PostgresMemory::new(pool.clone());
    let owner = Owner::new("alice".to_owned())?;
    let key = "claude-code:legacy-watermark";
    let keep = observed(
        batch(key, vec![message("first", "2026-01-02T01:00:00Z")?]),
        "Stop",
        "2026-01-02T01:10:00Z",
        false,
    )?;
    let id: Uuid = store
        .capture(keep, &owner, None)
        .await?
        .conversation_id
        .parse()?;
    sqlx::query("DELETE FROM rh_summary_state WHERE conversation_id = $1")
        .bind(id)
        .execute(&pool)
        .await?;
    let old_end = observed(
        batch(key, vec![]),
        "SessionEnd",
        "2026-01-02T01:09:00Z",
        true,
    )?;
    store.capture(old_end, &owner, None).await?;
    assert!(!store.summary_eligible(id, 0).await?);
    assert!(
        store
            .summary_state(id)
            .await?
            .ok_or("legacy state")?
            .historical_only
    );

    store
        .mark_summary_settled(key, &owner)
        .await?
        .ok_or("first mark")?;
    store
        .trash_session(key, Some(owner.as_str()), Timestamp::now())
        .await?;
    assert!(!store.summary_eligible(id, 0).await?);
    store.restore_session(key).await?;
    assert!(store.summary_eligible(id, 0).await?);

    sqlx::query("UPDATE rh_summary_state SET summarized_through = 0, failed_revision = 0 WHERE conversation_id = $1")
        .bind(id).execute(&pool).await?;
    store
        .capture(
            batch(key, vec![message("late", "2026-01-01T00:00:00Z")?]),
            &owner,
            None,
        )
        .await?;
    let state = store.summary_state(id).await?.ok_or("reopened state")?;
    assert_eq!(state.input_revision, 1);
    assert_eq!(state.summarized_through, Some(0));
    assert_eq!(state.failed_revision, None);
    assert_eq!(state.settled_at, None);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn opening_is_visible_only_to_owner_and_maintenance(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(crate::common::probe(&pool).await?);
    let alice = Owner::new("alice".to_owned())?;
    let bob = Owner::new("bob".to_owned())?;
    let id: Uuid = Scope::Viewer(Viewer::new(alice.clone(), Vec::new()))
        .run(store.capture(
            batch(
                "cli:invented-private",
                vec![message("invented", "2026-01-02T01:00:00Z")?],
            ),
            &alice,
            None,
        ))
        .await?
        .conversation_id
        .parse()?;
    let stranger = Scope::Viewer(Viewer::new(bob, vec![crate::visibility::household()?]));
    assert!(
        stranger
            .clone()
            .run(store.opening_title(id))
            .await?
            .is_none()
    );
    sqlx::query("UPDATE rh_conversations SET visibility = 'household' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await?;
    assert!(stranger.run(store.opening_title(id)).await?.is_none());
    assert!(
        Scope::Viewer(Viewer::new(alice, Vec::new()))
            .run(store.opening_title(id))
            .await?
            .is_some()
    );
    assert!(
        Scope::Maintenance
            .run(store.opening_title(id))
            .await?
            .is_some()
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn pending_replays_add_positions_without_replacing_transcript_fields(
    pool: PgPool,
) -> TestResult {
    let store = PostgresMemory::new(pool);
    testkit::settlement::replay_pending(&store, &testkit::owner()?, false).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn cowork_hook_and_transcript_keep_distinct_event_ids(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool);
    let owner = testkit::owner()?;
    let key = "cowork:invented-claim";
    let [hook, transcript] = testkit::settlement::cowork_pair()?;
    let initial = store
        .capture(batch(key, vec![hook.clone()]), &owner, None)
        .await?;
    let id = initial.conversation_id.parse()?;
    for (message, inserted) in [(transcript.clone(), 1), (transcript, 0), (hook, 0)] {
        assert_eq!(
            store
                .capture(batch(key, vec![message]), &owner, None)
                .await?
                .inserted,
            inserted
        );
    }
    let input = store.live_input(id).await?.ok_or("summarizer input")?;
    let metadata = |source| {
        input
            .messages
            .iter()
            .filter_map(|row| row.metadata.as_ref())
            .find(|metadata| metadata.get_str("source") == Some(source))
    };
    assert_eq!(input.messages.len(), 2);
    assert_eq!(
        metadata("cowork-hook")
            .and_then(|row| row.get("ordinal"))
            .and_then(testkit::Json::as_u64),
        Some(1)
    );
    assert_eq!(
        metadata("cowork-transcript")
            .and_then(|row| row.get("session_jsonl_line"))
            .and_then(testkit::Json::as_u64),
        Some(8)
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn unpositioned_keyset_preserves_timestamp_precision(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let key = "claude-code:precise";
    let messages = testkit::settlement::precision_messages()?;
    store
        .capture(batch(key, messages), &testkit::owner()?, None)
        .await?;
    sqlx::query(
        "UPDATE rh_messages SET created_at = CASE metadata->>'event_id'
        WHEN 'second' THEN '2026-01-01T00:00:01Z'
        WHEN 'earlier' THEN '2026-01-01T00:00:01.123100Z'
        WHEN 'later' THEN '2026-01-01T00:00:01.123900Z'
        WHEN 'offset' THEN '2026-01-01T01:00:01.130+01:00' END::timestamptz",
    )
    .execute(&pool)
    .await?;
    testkit::settlement::precise_pages(&store, key).await
}
