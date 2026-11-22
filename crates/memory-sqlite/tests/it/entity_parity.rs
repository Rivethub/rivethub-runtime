use memory_sqlite::SqliteStore;
use testkit::entities;

use crate::common::{TestResult, fresh_store, instant};

#[tokio::test]
async fn entity_known_names_aliases_and_facts() -> TestResult {
    let fixture = fresh_store()?;
    entities::known::match_case(&fixture.store).await
}

#[tokio::test]
async fn entity_known_orders_verbatim_before_folded_then_by_age() -> TestResult {
    let fixture = fresh_store()?;
    entities::known::order_case(&fixture.store).await
}

#[tokio::test]
async fn entity_known_stops_at_limit() -> TestResult {
    let fixture = fresh_store()?;
    entities::known::limit_case(&fixture.store).await
}

#[tokio::test]
async fn entity_same_normalized_name_and_compatible_type() -> TestResult {
    let fixture = fresh_store()?;
    entities::candidates::name_pairs(&fixture.store).await
}

#[tokio::test]
async fn entity_repo_tail_matching() -> TestResult {
    let fixture = fresh_store()?;
    entities::candidates::repo_tails(&fixture.store).await
}

#[tokio::test]
async fn entity_concurrent_creation() -> TestResult {
    let fixture = fresh_store()?;
    entities::candidates::concurrent(&fixture.store).await
}

#[tokio::test]
async fn entity_rejects_another_entity_name_as_alias() -> TestResult {
    let fixture = fresh_store()?;
    entities::candidates::rejected_alias(&fixture.store).await
}

#[tokio::test]
async fn entity_creates_type_and_matches_plural() -> TestResult {
    let fixture = fresh_store()?;
    entities::candidates::new_kind(&fixture.store).await
}

#[tokio::test]
async fn entity_marker_type_alias_and_decision() -> TestResult {
    let fixture = fresh_store()?;
    entities::candidates::markers_and_type_aliases(&fixture.store).await
}

#[tokio::test]
async fn entity_matching_scores_agree() -> TestResult {
    let fixture = fresh_store()?;
    entities::candidates::matching_scores(&fixture.store).await
}

#[tokio::test]
async fn entity_merge_keeps_the_oldest() -> TestResult {
    let fixture = fresh_store()?;
    entities::merge::oldest_survives(&fixture.store).await
}

#[tokio::test]
async fn entity_rewrite_replaces_claim_and_records_mention() -> TestResult {
    let fixture = fresh_store()?;
    entities::rewrite::replacement_and_mention(&fixture.store).await
}

#[tokio::test]
async fn entity_rewrite_orders_late_claims() -> TestResult {
    let fixture = fresh_store()?;
    entities::rewrite::out_of_order(&fixture.store).await
}

#[tokio::test]
async fn entity_concurrent_rewrites_keep_history() -> TestResult {
    let fixture = fresh_store()?;
    entities::rewrite::concurrent(&fixture.store).await
}

#[tokio::test]
async fn entity_restating_a_fact_is_idempotent() -> TestResult {
    let fixture = fresh_store()?;
    entities::rewrite::restated_fact(&fixture.store).await
}

#[tokio::test]
async fn entity_transcript_sources_and_window() -> TestResult {
    let fixture = fresh_store()?;
    entities::transcript::sources_then_window(&fixture.store).await
}

#[tokio::test]
async fn entity_backfill_and_completion() -> TestResult {
    let fixture = fresh_store()?;
    entities::backfill_and_completion(&fixture.store).await
}

#[tokio::test]
async fn entity_stale_refresh() -> TestResult {
    let fixture = fresh_store()?;
    entities::stale_refresh(&fixture.store).await
}

#[tokio::test]
async fn entity_merge_binds_an_unbound_survivor() -> TestResult {
    let fixture = fresh_store()?;
    entities::merge::binds_unbound_survivor(&fixture.store).await
}

#[tokio::test]
async fn entity_rewrite_lease_waits_across_connections() -> TestResult {
    let (_directory, holder, waiter) = separate_stores()?;
    entities::leases::rewrite_waits_for_page(&holder, &waiter).await
}

#[tokio::test]
async fn entity_merge_waits_for_rewrites_across_connections() -> TestResult {
    let (_directory, holder, merger) = separate_stores()?;
    entities::leases::merge_waits_for_both_entities(&holder, &merger).await
}

fn separate_stores() -> TestResult<(tempfile::TempDir, SqliteStore, SqliteStore)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite_at(&path, instant("2026-11-01T00:00:00Z")?)?;
    let holder = SqliteStore::open(&path)?;
    let waiter = SqliteStore::open(&path)?;
    Ok((directory, holder, waiter))
}

#[tokio::test]
async fn entity_expired_lease_is_replaced_and_fenced() -> TestResult {
    use jiff::Timestamp;
    use kernel::wiki::entity_pass::{EntityStore, MentionNote};
    use std::time::Duration;
    use tokio::time::timeout;
    use uuid::Uuid;

    let (_directory, holder, waiter) = separate_stores()?;
    let id = Uuid::new_v4();
    let mut stale = holder.lease_entity(id).await?;
    waiter
        .with_conn(move |conn| {
            conn.execute(
                "UPDATE rh_entity_leases SET expires_at = unixepoch() WHERE entity_id = ?1",
                [id.to_string()],
            )
            .map_err(crate::common::seed)
        })
        .await?;
    let limit = Duration::from_secs(5);
    let mut current = timeout(limit, waiter.lease_entity(id)).await??;
    let note = MentionNote {
        history: None,
        session_at: Timestamp::now(),
        session_id: Uuid::new_v4(),
        owner: "fixture".to_owned(),
    };
    let result = stale.record_mention(&note, note.session_at).await;
    let error = result.expect_err("expired owner wrote a mention");
    assert!(error.to_string().contains("entity lease is no longer held"));
    stale.release().await?;
    assert_eq!(current.applied_through().await?, None);
    drop(current);
    let mut next = timeout(limit, holder.lease_entity(id)).await??;
    next.release().await?;
    Ok(())
}

#[tokio::test]
async fn entity_live_lease_renews_while_rewrite_is_idle() -> TestResult {
    use kernel::wiki::entity_pass::EntityStore;
    use std::time::Duration;
    use tokio::time::{advance, pause, resume, timeout};
    use uuid::Uuid;

    let (_directory, holder, observer) = separate_stores()?;
    let id = Uuid::new_v4();
    pause();
    let mut lease = holder.lease_entity(id).await?;
    let short_expiry = observer
        .with_conn(move |conn| {
            conn.query_row(
                "UPDATE rh_entity_leases SET expires_at = unixepoch() + 30 WHERE entity_id = ?1
                 RETURNING expires_at",
                [id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(crate::common::seed)
        })
        .await?;
    advance(Duration::from_secs(15)).await;
    resume();
    timeout(Duration::from_secs(5), async {
        loop {
            let expires = observer
                .with_conn(move |conn| {
                    conn.query_row(
                        "SELECT expires_at FROM rh_entity_leases WHERE entity_id = ?1",
                        [id.to_string()],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(crate::common::seed)
                })
                .await?;
            if expires > short_expiry {
                return Ok::<_, memory_sqlite::SqliteError>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    lease.release().await?;
    Ok(())
}
