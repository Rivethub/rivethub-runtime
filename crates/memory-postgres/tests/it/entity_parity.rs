use kernel::tenancy::Scope;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use testkit::entities;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_known_names_aliases_and_facts(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::known::match_case(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_known_orders_verbatim_before_folded_then_by_age(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::known::order_case(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_known_stops_at_limit(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::known::limit_case(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_same_normalized_name_and_compatible_type(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::candidates::name_pairs(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_repo_tail_matching(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::candidates::repo_tails(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_concurrent_creation(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::candidates::concurrent(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_rejects_another_entity_name_as_alias(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::candidates::rejected_alias(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_creates_type_and_matches_plural(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::candidates::new_kind(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_marker_type_alias_and_decision(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::candidates::markers_and_type_aliases(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_matching_scores_agree(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::candidates::matching_scores(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_merge_keeps_the_oldest(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::merge::oldest_survives(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_rewrite_replaces_claim_and_records_mention(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::rewrite::replacement_and_mention(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_rewrite_orders_late_claims(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::rewrite::out_of_order(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_concurrent_rewrites_keep_history(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::rewrite::concurrent(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_restating_a_fact_is_idempotent(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::rewrite::restated_fact(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_transcript_sources_and_window(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::transcript::sources_then_window(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_backfill_and_completion(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::backfill_and_completion(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_stale_refresh(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::stale_refresh(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_merge_binds_an_unbound_survivor(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool).with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(entities::merge::binds_unbound_survivor(&store))
        .await
}
