#![cfg(test)]

use chrono::DateTime;
use chrono::Utc;
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
async fn the_same_normalized_name_is_one_entity_and_a_person_stays_apart(
    pool: PgPool,
) -> TestResult {
    Scope::Maintenance.run(name_pairs(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_repo_tail_binds_only_when_it_is_long_enough(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(repo_tails(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn concurrent_creation_of_one_name_is_one_entity(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(concurrent(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_alias_that_is_another_entity_name_is_rejected(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(rejected_alias(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_new_kind_is_a_type_and_a_plural_uses_the_existing_one(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(new_kind(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_hostname_on_the_page_finds_the_other_name(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(hostname_match(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_type_alias_and_its_decision_are_stored(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(stored_type(pool)).await
}

async fn new_kind(pool: PgPool) -> TestResult {
    let memory = memory(&pool).await?;
    let leaf = leaf();
    let fleet = commit(&memory, &leaf, "north", "Fleets", &[], None).await?;
    assert_eq!(fleet.type_name, "Fleets");
    assert_eq!(fleet.type_slug, "fleets");
    let host = commit(&memory, &leaf, "box", "host", &[], None).await?;
    assert_eq!(host.type_name, "Hosts");
    assert_eq!(host.type_slug, "hosts");
    let extra: Option<String> =
        sqlx::query_scalar("SELECT slug FROM rh_topics WHERE kind = 'type' AND slug = 'host'")
            .fetch_optional(&pool)
            .await?;
    assert!(extra.is_none());
    Ok(())
}

async fn name_pairs(pool: PgPool) -> TestResult {
    let memory = memory(&pool).await?;
    let leaf = leaf();
    let pairs = [
        (
            "Hugging Face",
            "Organizations",
            "Hugging Face",
            "Software",
            true,
        ),
        ("datahub", "Hosts", "datahub", "Software", true),
        ("rivet-glm", "Agents", "rivet-glm", "Software", true),
        ("rivet-claude", "Agents", "rivet-claude", "Hosts", true),
        ("T3 Code", "Software", "T3 Code", "Projects", true),
        ("Grok", "Agents", "Grok", "Software", true),
        (
            "transformers",
            "Libraries and frameworks",
            "transformers",
            "Software",
            true,
        ),
        ("omarchy", "Hosts", "Omarchy", "Software", true),
        (
            "windowsxp811203",
            "People",
            "windowsxp811203",
            "Organizations",
            false,
        ),
    ];
    for (left_name, left_type, right_name, right_type, same) in pairs {
        let first = commit(&memory, &leaf, left_name, left_type, &[], None).await?;
        assert!(
            contains(&memory, &[right_name.to_owned()], right_type, first.id).await? == same,
            "{left_name} [{left_type}] was offered={same}"
        );
        let second = commit(&memory, &leaf, right_name, right_type, &[], None).await?;
        assert_eq!(second.id == first.id, same, "{left_name} [{left_type}]");
        if same {
            assert_eq!(second.type_name, left_type, "{left_name} changed type");
        }
    }
    let glm = commit(&memory, &leaf, "rivet-glm", "Hosts", &[], None).await?;
    let glm_software = commit(&memory, &leaf, "rivet-glm", "Software", &[], None).await?;
    assert_eq!(glm.id, glm_software.id);
    let grok = commit(&memory, &leaf, "Grok", "Models", &[], None).await?;
    let grok_agent = commit(&memory, &leaf, "Grok", "Agents", &[], None).await?;
    assert_eq!(grok.id, grok_agent.id);

    let host = commit(&memory, &leaf, "sparehub", "Hosts", &[], None).await?;
    let bound = commit(&memory, &leaf, "Spare Hub", "Services", &[], Some(host.id)).await?;
    assert_eq!(bound.id, host.id);
    assert_eq!(bound.type_name, "Hosts");
    assert!(bound.aliases.iter().any(|alias| alias == "Spare Hub"));
    Ok(())
}

async fn repo_tails(pool: PgPool) -> TestResult {
    let memory = memory(&pool).await?;
    let leaf = leaf();
    let repo = commit(&memory, &leaf, "acme/datahub", "Repos", &[], None).await?;
    assert!(contains(&memory, &["datahub".to_owned()], "Services", repo.id).await?);
    let bound = commit(&memory, &leaf, "datahub", "Services", &[], None).await?;
    assert_eq!(bound.id, repo.id);
    assert_eq!(bound.type_name, "Repos");

    let long = "bottlecapai/ThinkingCap-Qwen3.8-Flash-Next";
    let model_repo = commit(&memory, &leaf, long, "Repos", &[], None).await?;
    assert!(
        contains(
            &memory,
            &["ThinkingCap".to_owned()],
            "Models",
            model_repo.id
        )
        .await?,
        "ThinkingCap is a fuzzy neighbour of the longer repo name"
    );
    let thinking = commit(&memory, &leaf, "ThinkingCap", "Models", &[], None).await?;
    assert_ne!(
        thinking.id, model_repo.id,
        "a short token is not the repo tail, so commit does not merge them"
    );

    let web = commit(&memory, &leaf, "web", "Services", &[], None).await?;
    let owned = commit(&memory, &leaf, "acme/web", "Repos", &[], None).await?;
    assert_ne!(owned.id, web.id);
    Ok(())
}

async fn concurrent(pool: PgPool) -> TestResult {
    let memory = memory(&pool).await?;
    let left_leaf = leaf();
    let right_leaf = leaf();
    let (left, right) = tokio::join!(
        commit(
            &memory,
            &left_leaf,
            "Hugging Face",
            "Organizations",
            &[],
            None
        ),
        commit(&memory, &right_leaf, "Hugging Face", "Software", &[], None),
    );
    let left = left?;
    let right = right?;
    assert_eq!(left.id, right.id);
    let again = commit(&memory, &left_leaf, "huggingface", "Projects", &[], None).await?;
    assert_eq!(again.id, left.id);
    Ok(())
}

async fn rejected_alias(pool: PgPool) -> TestResult {
    let memory = memory(&pool).await?;
    let leaf = leaf();
    let nvidia = commit(&memory, &leaf, "nvidia", "Organizations", &[], None).await?;
    let card = commit(
        &memory,
        &leaf,
        "V100",
        "Hardware",
        &["nvidia".to_owned()],
        None,
    )
    .await?;
    assert_ne!(card.id, nvidia.id);
    assert!(
        card.aliases.iter().all(|alias| alias != "nvidia"),
        "V100 claimed nvidia: {:?}",
        card.aliases
    );
    Ok(())
}

async fn hostname_match(pool: PgPool) -> TestResult {
    let memory = memory(&pool).await?;
    let leaf = leaf();
    let gerty = commit(&memory, &leaf, "gerty", "Hosts", &["G9".to_owned()], None).await?;
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
    .bind(gerty.id)
    .bind(at("2026-11-13T02:00:00Z"))
    .bind(Uuid::new_v4())
    .bind(summary)
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO rh_wiki_topics (slug, title, topic_id, current_state, article, owner_user_id)
         VALUES ('gerty-page', 'gerty', $1, 'box9 is this machine', 'also called G9', 'owner')",
    )
    .bind(gerty.id)
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
    let page = found.iter().find(|item| item.id == gerty.id);
    let Some(page) = page else {
        panic!("box9 did not find gerty");
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
    assert!(alias.iter().any(|item| item.id == gerty.id));
    Ok(())
}

async fn stored_type(pool: PgPool) -> TestResult {
    let memory = memory(&pool).await?;
    let leaf = leaf();
    let host = commit(&memory, &leaf, "gerty", "Hosts", &[], None).await?;
    assert_eq!(host.type_name, "Hosts");
    memory.remember_type_alias("Hosts", "machine").await?;
    memory.remember_type_alias("Hosts", "machine").await?;
    let briefs = memory.type_briefs().await?;
    let hosts = briefs
        .iter()
        .find(|brief| brief.name == "Hosts")
        .expect("Hosts");
    assert!(hosts.aliases.iter().any(|alias| alias == "machine"));
    assert!(hosts.examples.iter().any(|example| example == "gerty"));
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
        candidates: vec!["Hosts (examples: gerty)".to_owned()],
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
    .bind(at("2026-11-13T00:00:00Z"))
    .bind(at("2026-11-13T01:00:00Z"))
    .fetch_one(pool)
    .await
}

async fn contains(
    memory: &PostgresMemory,
    names: &[String],
    type_name: &str,
    id: Uuid,
) -> Result<bool, Box<dyn std::error::Error>> {
    let found = memory
        .match_candidates(&CandidateQuery {
            names: names.to_vec(),
            type_name: type_name.to_owned(),
            embedding: None,
            markers: Vec::new(),
        })
        .await?;
    Ok(found.iter().any(|item| item.id == id))
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

fn leaf() -> EntityLeaf {
    EntityLeaf {
        id: Uuid::new_v4(),
        conversation_id: None,
        content: "leaf".to_owned(),
        start: at("2026-11-07T00:00:00Z"),
        end: at("2026-11-07T01:00:00Z"),
        owner: Some("owner".to_owned()),
    }
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).map_or(DateTime::UNIX_EPOCH, |time| time.with_timezone(&Utc))
}
