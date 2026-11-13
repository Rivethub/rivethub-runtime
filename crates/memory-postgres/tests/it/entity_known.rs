#![cfg(test)]

use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use kernel::tenancy::Scope;
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityStore;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::KNOWN_LIMIT;
use kernel::wiki::entity_pass::KnownEntity;
use kernel::wiki::entity_pass::PreparedEntity;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn known_entities_match_names_aliases_and_skip_absences(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(match_case(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn known_entities_orders_verbatim_before_folded_then_by_age(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(order_case(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn known_entities_stops_at_the_known_limit(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(limit_case(pool)).await
}

async fn match_case(pool: PgPool) -> TestResult {
    let (memory, scoped) = memory(&pool).await?;
    let summary = seed_summary(&scoped).await?;
    let datahub = commit(&memory, "datahub", "Hosts", &[], "2026-11-01T00:00:00Z").await?;
    let aliased = commit(
        &memory,
        "sparehost",
        "Hosts",
        &["comfyui"],
        "2026-11-01T00:01:00Z",
    )
    .await?;
    let folded = commit(&memory, "gpubox", "Hosts", &[], "2026-11-01T00:02:00Z").await?;
    let repo = commit(
        &memory,
        "acme/Widget-Box",
        "Repos",
        &[],
        "2026-11-01T00:03:00Z",
    )
    .await?;
    let alias_folded = commit(
        &memory,
        "sparepump",
        "Hosts",
        &["Nfs-Server"],
        "2026-11-01T00:04:00Z",
    )
    .await?;
    let absent = commit(
        &memory,
        "notmentionedxyz",
        "Hosts",
        &[],
        "2026-11-01T00:05:00Z",
    )
    .await?;
    seed_fact(&scoped, datahub.id, summary).await?;
    let known = known_in(
        &memory,
        "Gpu-Box hosts datahub beside comfyui. We shipped ACME/widget-box after nfsserver came up.",
    )
    .await?;
    let found = named(&known, "datahub").ok_or("datahub missing")?;
    assert_eq!(found.id, datahub.id);
    assert_eq!(found.type_name, "Hosts");
    assert!(
        found
            .facts
            .iter()
            .any(|fact| { fact.attribute == "ip" && fact.value == "11.22.33.44" })
    );
    let host = named(&known, "sparehost").ok_or("alias missing")?;
    assert_eq!(host.id, aliased.id);
    assert!(host.aliases.iter().any(|alias| alias == "comfyui"));
    assert_eq!(named(&known, "gpubox").map(|item| item.id), Some(folded.id));
    assert_eq!(
        named(&known, "acme/Widget-Box").map(|item| item.id),
        Some(repo.id)
    );
    let pump = named(&known, "sparepump").ok_or("folded alias missing")?;
    assert_eq!(pump.id, alias_folded.id);
    assert!(pump.aliases.iter().any(|alias| alias == "Nfs-Server"));
    assert!(known.iter().all(|item| item.id != absent.id));
    Ok(())
}

async fn order_case(pool: PgPool) -> TestResult {
    let (memory, _) = memory(&pool).await?;
    commit(&memory, "beta-two", "Hosts", &[], "2026-11-01T00:00:00Z").await?;
    commit(&memory, "alphaone", "Hosts", &[], "2026-11-02T00:00:00Z").await?;
    commit(&memory, "gammaone", "Hosts", &[], "2026-11-03T00:00:00Z").await?;
    let known = known_in(&memory, "alphaone met gammaone near betatwo").await?;
    let names: Vec<&str> = known.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, vec!["alphaone", "gammaone", "beta-two"]);
    Ok(())
}

async fn limit_case(pool: PgPool) -> TestResult {
    let (memory, _) = memory(&pool).await?;
    let limit = usize::try_from(KNOWN_LIMIT).unwrap_or(60);
    let total = limit + 10;
    let mut document = String::new();
    for index in 0..total {
        let name = format!("lim{index:02}");
        document.push_str(&name);
        document.push(' ');
        let when =
            at("2026-11-01T00:00:00Z") + Duration::seconds(i64::try_from(index).unwrap_or(0));
        commit(&memory, &name, "Hosts", &[], &when.to_rfc3339()).await?;
    }
    let known = known_in(&memory, &document).await?;
    assert_eq!(known.len(), limit);
    assert_eq!(known.first().map(|item| item.name.as_str()), Some("lim00"));
    let last = format!("lim{index:02}", index = limit - 1);
    assert_eq!(
        known.get(limit - 1).map(|item| item.name.as_str()),
        Some(last.as_str())
    );
    let dropped = format!("lim{index:02}", index = total - 1);
    assert!(known.iter().all(|item| item.name != dropped));
    Ok(())
}

async fn memory(pool: &PgPool) -> Result<(PostgresMemory, PgPool), sqlx::Error> {
    let scoped = memory_postgres::scoped(PgPoolOptions::new().max_connections(4))
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    Ok((PostgresMemory::new(scoped.clone()), scoped))
}

async fn known_in(
    memory: &PostgresMemory,
    content: &str,
) -> Result<Vec<KnownEntity>, Box<dyn std::error::Error>> {
    let leaf = leaf(content, "2026-11-09T00:00:00Z");
    let observed = memory.observe_context(std::slice::from_ref(&leaf)).await?;
    Ok(observed.known)
}

async fn commit(
    memory: &PostgresMemory,
    name: &str,
    type_name: &str,
    aliases: &[&str],
    start: &str,
) -> Result<EntityView, Box<dyn std::error::Error>> {
    let prepared = PreparedEntity {
        entity_ref: None,
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        facts: Vec::new(),
        history: Vec::new(),
    };
    let leaf = leaf(name, start);
    memory
        .commit(
            std::slice::from_ref(&leaf),
            &prepared,
            at("2026-11-08T00:00:00Z"),
        )
        .await?
        .ok_or_else(|| "commit skipped".into())
}

async fn seed_summary(pool: &PgPool) -> Result<Uuid, sqlx::Error> {
    let conversation: Uuid = sqlx::query_scalar(
        "INSERT INTO rh_conversations (session_key, agent, channel, title, owner_user_id)
         VALUES ('claude-code:entity-known', 'rivet', 'cli', 'known', 'owner') RETURNING id",
    )
    .fetch_one(pool)
    .await?;
    sqlx::query_scalar(
        "INSERT INTO rh_summaries (conversation_id, content, kind, earliest_at, latest_at, owner_user_id)
         VALUES ($1, 'leaf', 'leaf', $2, $3, 'owner') RETURNING id",
    )
    .bind(conversation)
    .bind(at("2026-11-01T00:00:00Z"))
    .bind(at("2026-11-01T01:00:00Z"))
    .fetch_one(pool)
    .await
}

async fn seed_fact(pool: &PgPool, entity: Uuid, summary: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "WITH fact AS (
            INSERT INTO rh_entity_facts (id, topic_id, key, created_at)
            VALUES ($1, $2, 'ip', $3)
            RETURNING id
         )
         INSERT INTO rh_entity_fact_claims
            (id, fact_id, value, actor, pinned, session_id, summary_id, at, recorded_at, owner_user_id)
         SELECT $4, fact.id, '11.22.33.44', 'wiki-writer', false,
                (SELECT c.session_key FROM rh_summaries s JOIN rh_conversations c ON c.id = s.conversation_id WHERE s.id = $5),
                $5, $3, $3, 'owner'
           FROM fact",
    )
    .bind(Uuid::new_v4())
    .bind(entity)
    .bind(at("2026-11-01T00:30:00Z"))
    .bind(Uuid::new_v4())
    .bind(summary)
    .execute(pool)
    .await
    .map(drop)
}

fn named<'a>(known: &'a [KnownEntity], name: &str) -> Option<&'a KnownEntity> {
    known.iter().find(|item| item.name == name)
}

fn leaf(content: &str, start: &str) -> EntityLeaf {
    EntityLeaf {
        id: Uuid::new_v4(),
        conversation_id: None,
        content: content.to_owned(),
        start: at(start),
        end: at(start),
        owner: Some("owner".to_owned()),
    }
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).map_or(DateTime::UNIX_EPOCH, |time| time.with_timezone(&Utc))
}
