#![cfg(test)]

use jiff::Timestamp;
use kernel::tenancy::Scope;
use kernel::wiki::entity_pass::CandidateQuery;
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityStore;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::MatchDecision;
use kernel::wiki::entity_pass::PreparedEntity;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use sqlx::Row as _;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_hostname_on_the_page_finds_the_other_name(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(hostname_match(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_type_alias_and_its_decision_are_stored(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(stored_type(pool)).await
}

async fn hostname_match(pool: PgPool) -> TestResult {
    let memory = memory(&pool).await?;
    let leaf = leaf(&pool).await?;
    let hally = commit(&memory, &leaf, "hally", "Hosts", &["G9".to_owned()], None).await?;
    let summary = seed_summary(&pool).await?;
    sqlx::query(
        "WITH fact AS (
            INSERT INTO rh_entity_facts (id, topic_id, key, created_at)
            VALUES ($1, $2, 'hostname', $3)
            RETURNING id
         )
         INSERT INTO rh_entity_fact_claims
            (id, fact_id, value, actor, pinned, session_id, summary_id, at, recorded_at,
             owner_user_id, settled)
         SELECT $4, fact.id, 'box9', 'wiki-writer', false, 'claude-code:entity-decide', $5,
                $3, $3, 'owner', true
           FROM fact",
    )
    .bind(Uuid::new_v4())
    .bind(hally.id)
    .bind(memory_postgres::time::PgTimestamp::from(at(
        "2026-11-13T02:00:00Z",
    )))
    .bind(Uuid::new_v4())
    .bind(summary)
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO rh_wiki_topics (slug, title, topic_id, current_state, article, owner_user_id)
         VALUES ('hally-page', 'hally', $1, 'box9 is this machine', 'also called G9', 'owner')",
    )
    .bind(hally.id)
    .execute(&pool)
    .await?;
    let found = memory
        .match_candidates(&CandidateQuery {
            names: vec!["box9".to_owned()],
            type_name: "Hosts".to_owned(),
            embedding: None,
            markers: vec!["box9".to_owned()],
        })
        .await?;
    let page = found.iter().find(|item| item.id == hally.id);
    let Some(page) = page else {
        panic!("box9 did not find hally");
    };
    assert!(
        page.summary.contains("box9 is this machine"),
        "{}",
        page.summary
    );
    let alias = memory
        .match_candidates(&CandidateQuery {
            names: vec!["box9".to_owned(), "G9".to_owned()],
            type_name: "Hosts".to_owned(),
            embedding: None,
            markers: Vec::new(),
        })
        .await?;
    assert!(alias.iter().any(|item| item.id == hally.id));
    Ok(())
}

async fn stored_type(pool: PgPool) -> TestResult {
    let memory = memory(&pool).await?;
    let leaf = leaf(&pool).await?;
    let host = commit(&memory, &leaf, "hally", "Hosts", &[], None).await?;
    assert_eq!(host.type_name, "Hosts");
    memory.remember_type_alias("Hosts", "machine").await?;
    memory.remember_type_alias("Hosts", "machine").await?;
    let briefs = memory.type_briefs().await?;
    let hosts = briefs
        .iter()
        .find(|brief| brief.name == "Hosts")
        .expect("Hosts");
    assert!(hosts.aliases.iter().any(|alias| alias == "machine"));
    assert!(hosts.examples.iter().any(|example| example == "hally"));
    let aliases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM rh_topic_aliases WHERE kind = 'type' AND alias = 'machine'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(aliases, 1);
    let summary = seed_summary(&pool).await?;
    let decision = MatchDecision {
        kind: "type".to_owned(),
        proposed_name: "machine".to_owned(),
        proposed_type: "type".to_owned(),
        candidates: vec!["Hosts (examples: hally)".to_owned()],
        verdict: "same".to_owned(),
        choice: "0".to_owned(),
        confidence: 0.93,
        model: "stub".to_owned(),
        entity_id: None,
    };
    memory
        .record_decision(summary, &decision, at("2026-11-13T03:00:00Z"))
        .await?;
    let row = sqlx::query(
        "SELECT kind, proposed_name, verdict, model, candidates::text AS candidates
           FROM rh_entity_decisions WHERE summary_id = $1",
    )
    .bind(summary)
    .fetch_one(&pool)
    .await?;
    let kind: String = row.try_get("kind")?;
    let name: String = row.try_get("proposed_name")?;
    let verdict: String = row.try_get("verdict")?;
    let model: String = row.try_get("model")?;
    let candidates: String = row.try_get("candidates")?;
    assert_eq!(kind, "type");
    assert_eq!(name, "machine");
    assert_eq!(verdict, "same");
    assert_eq!(model, "stub");
    assert!(candidates.contains("Hosts"));
    Ok(())
}

async fn seed_summary(pool: &PgPool) -> Result<Uuid, sqlx::Error> {
    let conversation: Uuid = sqlx::query_scalar(
        "INSERT INTO rh_conversations (session_key, agent, channel, title, owner_user_id)
         VALUES ('claude-code:entity-decide', 'rivet', 'cli', 'decide', 'owner') RETURNING id",
    )
    .fetch_one(pool)
    .await?;
    sqlx::query_scalar(
        "INSERT INTO rh_summaries (conversation_id, content, kind, earliest_at, latest_at, owner_user_id)
         VALUES ($1, 'leaf', 'leaf', $2, $3, 'owner') RETURNING id",
    )
    .bind(conversation)
    .bind(memory_postgres::time::PgTimestamp::from(at("2026-11-13T00:00:00Z")))
    .bind(memory_postgres::time::PgTimestamp::from(at("2026-11-13T01:00:00Z")))
    .fetch_one(pool)
    .await
}

async fn memory(pool: &PgPool) -> Result<PostgresMemory, sqlx::Error> {
    let scoped = memory_postgres::scoped(PgPoolOptions::new().max_connections(4))
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    Ok(PostgresMemory::new(scoped))
}

async fn commit(
    memory: &PostgresMemory,
    leaf: &EntityLeaf,
    name: &str,
    type_name: &str,
    aliases: &[String],
    entity_ref: Option<Uuid>,
) -> Result<EntityView, Box<dyn std::error::Error>> {
    let prepared = PreparedEntity {
        entity_ref,
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        aliases: aliases.to_vec(),
        facts: Vec::new(),
        history: Vec::new(),
    };
    memory
        .commit(
            std::slice::from_ref(leaf),
            &prepared,
            at("2026-11-07T02:00:00Z"),
        )
        .await?
        .ok_or_else(|| "commit skipped".into())
}

async fn leaf(pool: &PgPool) -> Result<EntityLeaf, sqlx::Error> {
    let leaf = EntityLeaf {
        delivery: None,
        id: Uuid::new_v4(),
        conversation_id: None,
        content: "leaf".to_owned(),
        start: at("2026-11-07T00:00:00Z"),
        end: at("2026-11-07T01:00:00Z"),
        owner: Some("owner".to_owned()),
    };
    crate::common::seed_entity_leaf(pool, &leaf).await?;
    Ok(leaf)
}

fn at(text: &str) -> Timestamp {
    wire::time::parse_rfc3339(text).unwrap_or(Timestamp::UNIX_EPOCH)
}
