use jiff::Timestamp;
use kernel::memory::settlement::SettleStatus;
use kernel::memory::{MemoryStore as _, SummaryStore as _};
use memory_sqlite::SqliteStore;
use testkit::settlement::{TestResult, batch, expire, message, position_fills, revision_lifecycle};
use uuid::Uuid;
use wire::message::Role;
use wire::time::TimestampFormat as _;

fn store(path: &std::path::Path) -> Result<SqliteStore, Box<dyn std::error::Error>> {
    migrate::sqlite(path)?;
    Ok(SqliteStore::open(path)?)
}

#[tokio::test]
async fn revisions_and_position_fills() -> TestResult {
    let home = tempfile::tempdir()?;
    let store = store(&home.path().join("memory.sqlite"))?;
    let owner = testkit::owner()?;
    revision_lifecycle(&store, &owner).await?;
    position_fills(&store, &owner).await
}

#[tokio::test]
async fn result_fills_and_persistence() -> TestResult {
    let home = tempfile::tempdir()?;
    let path = home.path().join("memory.sqlite");
    let alice = testkit::owner()?;
    let now = Timestamp::now();
    let mut tool = message("blade", "2026-01-02T01:00:00Z")?;
    tool.role = Role::Tool;
    tool.tool_name = Some("test-tool".to_owned());
    let mut metadata = wire::JsonObject::default();
    metadata.insert("pending_result", true)?;
    tool.metadata = Some(metadata);
    let local = store(&path)?;
    let id: Uuid = local
        .capture(batch("cli:blade", vec![tool.clone()]), &alice, None, now)
        .await?
        .conversation_id
        .parse()?;
    local
        .mark_summary_settled("cli:blade", &alice)
        .await?
        .ok_or("mark")?;
    drop(local);

    let local = store(&path)?;
    assert!(local.summary_eligible(id, 1).await?);
    let mut empty_fill = tool.clone();
    empty_fill.tool_result = Some(String::new());
    empty_fill.metadata = None;
    local
        .capture(batch("cli:blade", vec![empty_fill]), &alice, None, now)
        .await?;
    assert!(local.summary_eligible(id, 1).await?);

    let other: Uuid = local
        .capture(batch("cli:shaft", vec![tool.clone()]), &alice, None, now)
        .await?
        .conversation_id
        .parse()?;
    local
        .mark_summary_settled("cli:shaft", &alice)
        .await?
        .ok_or("mark")?;
    tool.tool_result = Some("A round wooden shaft.".to_owned());
    tool.metadata = None;
    local
        .capture(batch("cli:shaft", vec![tool]), &alice, None, now)
        .await?;
    assert!(!local.summary_eligible(other, 1).await?);
    assert!(!local.summary_eligible(other, 2).await?);
    assert_eq!(
        local
            .summary_state(other)
            .await?
            .ok_or("fill state")?
            .input_revision,
        2
    );

    local.with_conn(move |conn| {
        conn.execute("UPDATE rh_summary_state SET summarized_through = input_revision, failed_revision = input_revision WHERE conversation_id = ?1", [id.to_string()])
            .map_err(|source| memory_sqlite::SqliteError::Query { operation: "test completed revision", source })?;
        Ok(())
    }).await?;
    assert_eq!(
        local
            .mark_summary_settled("cli:blade", &alice)
            .await?
            .ok_or("completed")?
            .status,
        SettleStatus::AlreadySummarized
    );
    assert!(!local.summary_eligible(id, 1).await?);
    assert_eq!(
        local
            .summary_state(id)
            .await?
            .ok_or("state")?
            .failed_revision,
        None
    );
    Ok(())
}

#[tokio::test]
async fn historical_and_live_expiry_across_pages() -> TestResult {
    let home = tempfile::tempdir()?;
    let local = store(&home.path().join("memory.sqlite"))?;
    let alice = testkit::owner()?;
    let now = Timestamp::now();
    let live: Uuid = local
        .capture(
            batch("cli:live", vec![message("live", "2026-01-02T01:00:00Z")?]),
            &alice,
            None,
            now,
        )
        .await?
        .conversation_id
        .parse()?;
    let historical: Uuid = local
        .capture(
            batch(
                "cli:past",
                vec![message("past", "2026-01-02T01:00:00.123456789Z")?],
            ),
            &alice,
            None,
            now,
        )
        .await?
        .conversation_id
        .parse()?;
    local.with_conn(move |conn| {
        conn.execute("DELETE FROM rh_summary_capture_clock WHERE conversation_id = ?1", [historical.to_string()])
            .map_err(crate::common::seed)?;
        conn.execute("DELETE FROM rh_summary_state WHERE conversation_id = ?1", [historical.to_string()])
            .map_err(|source| memory_sqlite::SqliteError::Query { operation: "test historical state", source })?;
        conn.execute("UPDATE ros_conversations SET updated_at = '2099-01-02T01:00:00.000Z' WHERE id = ?1", [historical.to_string()])
            .map_err(|source| memory_sqlite::SqliteError::Query { operation: "test unrelated update time", source })?;
        Ok(())
    }).await?;
    assert!(local.summary_state(historical).await?.is_none());
    assert!(!local.summary_eligible(live, 1).await?);
    expire(&local, now, 1).await?;
    assert!(!local.summary_eligible(live, 1).await?);
    assert!(local.summary_eligible(historical, 0).await?);
    let state = local.summary_state(historical).await?.ok_or("seed")?;
    assert!(state.historical_only);
    assert_eq!(
        state.last_input_at,
        "2026-01-02T01:00:00.123Z".parse::<Timestamp>()?
    );
    testkit::settlement::expiry_boundary(&local, live).await?;
    assert!(local.summary_eligible(live, 1).await?);
    Ok(())
}

#[tokio::test]
async fn settlement_reads_legacy_text_with_its_original_precision() -> TestResult {
    let home = tempfile::tempdir()?;
    let path = home.path().join("memory.sqlite");
    let local = store(&path)?;
    let now: Timestamp = "2026-01-02T01:00:00.123456789Z".parse()?;
    let id: Uuid = local
        .capture(
            batch(
                "cli:legacy-stamps",
                vec![message("legacy", "2026-01-02T01:00:00Z")?],
            ),
            &testkit::owner()?,
            None,
            now,
        )
        .await?
        .conversation_id
        .parse()?;
    assert_eq!(
        local
            .summary_state(id)
            .await?
            .ok_or("captured state")?
            .last_input_at,
        "2026-01-02T01:00:00.123Z".parse::<Timestamp>()?,
    );
    let conn = rusqlite::Connection::open(path)?;
    conn.execute(
        "UPDATE rh_summary_state SET last_input_at = '2026-01-02T02:00:00+01:00',
                last_observed_event_at = '2026-01-02T01:00:00.123456789Z' WHERE conversation_id = ?1",
        [id.to_string()],
    )?;
    let state = local.summary_state(id).await?.ok_or("legacy state")?;
    assert_eq!(
        state.last_input_at,
        "2026-01-02T01:00:00Z".parse::<Timestamp>()?
    );
    assert_eq!(state.last_observed_event_at, Some(now));
    conn.execute(
        "UPDATE rh_summary_state SET last_input_at = '9999-12-31T00:00:01Z' WHERE conversation_id = ?1",
        [id.to_string()],
    )?;
    assert!(local.summary_state(id).await.is_err());
    Ok(())
}

#[tokio::test]
async fn unpositioned_keyset_preserves_legacy_timestamp_precision() -> TestResult {
    let home = tempfile::tempdir()?;
    let local = store(&home.path().join("memory.sqlite"))?;
    let key = "claude-code:precise";
    let messages = testkit::settlement::precision_messages()?;
    local
        .capture(
            batch(key, messages),
            &testkit::owner()?,
            None,
            Timestamp::now(),
        )
        .await?;
    crate::common::execute(
        &local,
        "
        UPDATE ros_messages SET created_at = CASE json_extract(metadata, '$.event_id')
            WHEN 'second' THEN '2026-01-01T00:00:01Z'
            WHEN 'earlier' THEN '2026-01-01T00:00:01.123100001Z'
            WHEN 'later' THEN '2026-01-01T00:00:01.123100002Z'
            WHEN 'offset' THEN '2026-01-01 01:00:01.130+01'
        END
    ",
    )
    .await?;
    testkit::settlement::precise_pages(&local, key).await
}

#[tokio::test]
async fn expiry_recovers_the_capture_clock_when_state_is_missing() -> TestResult {
    let home = tempfile::tempdir()?;
    let local = store(&home.path().join("memory.sqlite"))?;
    let now = Timestamp::now().truncate_subseconds(3);
    let request = testkit::settlement::old_source_capture()?;
    let id: Uuid = local
        .capture(request, &testkit::owner()?, None, now)
        .await?
        .conversation_id
        .parse()?;
    local
        .with_conn(move |conn| {
            conn.execute(
                "DELETE FROM rh_summary_state WHERE conversation_id = ?1",
                [id.to_string()],
            )
            .map_err(crate::common::seed)?;
            Ok(())
        })
        .await?;
    testkit::settlement::missing_state_expiry(&local, id, now).await
}
