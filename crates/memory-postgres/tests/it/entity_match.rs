#![cfg(test)]

use chrono::DateTime;
use chrono::Utc;
use kernel::tenancy::Scope;
use kernel::wiki::entity_pass::CandidateQuery;
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityStore;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::PreparedEntity;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
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
        !contains(
            &memory,
            &["ThinkingCap".to_owned()],
            "Models",
            model_repo.id
        )
        .await?
    );
    let thinking = commit(&memory, &leaf, "ThinkingCap", "Models", &[], None).await?;
    assert_ne!(thinking.id, model_repo.id);

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
