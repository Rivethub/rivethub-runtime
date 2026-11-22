use testkit::entities;

use crate::common::{TestResult, fresh_store};

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
