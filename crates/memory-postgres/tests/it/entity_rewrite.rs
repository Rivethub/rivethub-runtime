#![cfg(test)]

use chrono::DateTime;
use chrono::Utc;
use kernel::facts::FactStore;
use kernel::review::ReviewKey;
use kernel::review::fact_reviews;
use kernel::tenancy::Scope;
use kernel::topics::TopicId;
use kernel::wiki::builder::BackfillBudget;
use kernel::wiki::entity_pass::EntityRewrite;
use kernel::wiki::entity_pass::EntityStore;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::FactState;
use kernel::wiki::entity_pass::LeafClock;
use kernel::wiki::entity_pass::LiveFact;
use kernel::wiki::entity_pass::MentionNote;
use kernel::wiki::entity_pass::PreparedFact;
use kernel::wiki::entity_pass::PreparedHistory;
use kernel::wiki::entity_pass::RetiredFact;
use kernel::wiki::entity_pass::RewriteFact;
use kernel::wiki::entity_pass::StoredRewrite;
use kernel::wiki::entity_pass::reconcile_facts;
use kernel::wiki::entity_pass::session_history;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_rewrite_replaces_the_fact_snapshot_and_appends_history_once(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(rewrite_case(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_empty_rewrite_mention_keeps_facts_appends_history_and_advances_the_mark(
    pool: PgPool,
) -> TestResult {
    Scope::Maintenance.run(mention_case(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn newest_then_oldest_keeps_the_newer_values_and_both_history_lines(
    pool: PgPool,
) -> TestResult {
    Scope::Maintenance.run(ordered_case(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn concurrent_rewrites_of_one_entity_keep_every_history_line(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(concurrent_case(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn backfill_lists_sessions_oldest_first(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(backfill_order(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_omitted_fact_is_carried_forward(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(carried_case(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_updated_attribute_supersedes_and_keeps_history(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(updated_case(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_retired_fact_is_superseded_with_its_reason(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(retired_case(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_renamed_attribute_ends_with_only_the_new_key(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(renamed_case(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_restated_fact_from_the_same_session_adds_no_claim(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(restated_case(pool)).await
}

async fn rewrite_case(pool: PgPool) -> TestResult {
    let conversation = seed_conversation(&pool).await?;
    let older = seed_leaf(&pool, conversation, at("2026-11-01T00:00:00Z")).await?;
    let newer = seed_leaf(&pool, conversation, at("2026-11-07T00:00:00Z")).await?;
    let entity = Uuid::new_v4();
    let slug = format!("gpubox-{entity}");
    seed_entity(&pool, entity, &slug).await?;
    seed_fact(&pool, entity, "ram", "87 GB available", older, conversation).await?;
    seed_fact(&pool, entity, "ip", "11.22.33.44", older, conversation).await?;
    let directory = tempfile::tempdir()?;
    let memory =
        PostgresMemory::new(pool.clone()).with_wiki_directory(directory.path().to_path_buf());
    let mut lease = memory.lease_entity(entity).await?;
    let first = rewrite_body(
        newer,
        conversation,
        at("2026-11-07T00:00:00Z"),
        "152 GB available",
        "Recorded 152 GB available.",
        true,
    );
    lease.apply(&first, at("2026-11-07T01:00:00Z")).await?;
    let second = rewrite_body(
        newer,
        conversation,
        at("2026-11-07T00:00:00Z"),
        "160 GB available",
        "Recorded 160 GB available.",
        false,
    );
    lease.apply(&second, at("2026-11-07T02:00:00Z")).await?;
    lease.release().await?;
    assert_replaced(&pool, entity, &slug, directory.path()).await
}

async fn mention_case(pool: PgPool) -> TestResult {
    let conversation = seed_conversation(&pool).await?;
    let older = seed_leaf(&pool, conversation, at("2026-11-01T00:00:00Z")).await?;
    let newer = seed_leaf(&pool, conversation, at("2026-11-07T00:00:00Z")).await?;
    let later = seed_leaf(&pool, conversation, at("2026-11-08T00:00:00Z")).await?;
    let entity = Uuid::new_v4();
    let slug = format!("gpubox-{entity}");
    seed_entity(&pool, entity, &slug).await?;
    seed_fact(&pool, entity, "ram", "87 GB available", older, conversation).await?;
    let directory = tempfile::tempdir()?;
    let memory =
        PostgresMemory::new(pool.clone()).with_wiki_directory(directory.path().to_path_buf());
    let body = rewrite_body(
        newer,
        conversation,
        at("2026-11-07T00:00:00Z"),
        "152 GB available",
        "Recorded 152 GB available.",
        false,
    );
    apply_once(&memory, entity, &body).await?;
    let mut before = facts_of(&pool, entity).await?;
    before.sort_by_key(|fact| fact.id);
    let note = MentionNote {
        history: Some(PreparedHistory {
            summary_id: later,
            conversation_id: Some(conversation),
            session_on: at("2026-11-08T00:00:00Z").date_naive(),
            sentence: "Seen again.".to_owned(),
        }),
        session_at: at("2026-11-08T00:00:00Z"),
        session_id: later,
        owner: "owner".to_owned(),
    };
    let mut lease = memory.lease_entity(entity).await?;
    let sha = lease
        .record_mention(&note, at("2026-11-08T01:00:00Z"))
        .await?;
    lease.release().await?;
    assert!(sha.is_some());
    let mut after = facts_of(&pool, entity).await?;
    after.sort_by_key(|fact| fact.id);
    assert_eq!(after, before);
    let history = history_of(&pool, entity).await?;
    assert_eq!(
        history,
        vec![
            "Recorded 152 GB available.".to_owned(),
            "Seen again.".to_owned(),
        ]
    );
    let summary =
        sqlx::query_scalar::<_, String>("SELECT current_state FROM rh_wiki_topics WHERE slug = $1")
            .bind(&slug)
            .fetch_one(&pool)
            .await?;
    assert_eq!(summary, "GPU host.");
    let mark = sqlx::query_as::<_, AppliedSeen>(
        "SELECT applied_through, summary_id FROM rh_entity_applied WHERE entity_id = $1",
    )
    .bind(entity)
    .fetch_one(&pool)
    .await?;
    assert_eq!(mark.summary_id, later);
    assert_eq!(mark.applied_through, at("2026-11-08T00:00:00Z"));
    let page = std::fs::read_to_string(directory.path().join(format!("topics/{slug}.md")))?;
    assert!(page.contains("Seen again."));
    assert!(page.contains("152 GB available"));
    assert!(page.contains("GPU host."));
    Ok(())
}

async fn ordered_case(pool: PgPool) -> TestResult {
    let conversation = seed_conversation(&pool).await?;
    let older = seed_leaf(&pool, conversation, at("2026-11-01T00:00:00Z")).await?;
    let newer = seed_leaf(&pool, conversation, at("2026-11-07T00:00:00Z")).await?;
    let entity = Uuid::new_v4();
    let slug = format!("gpubox-{entity}");
    seed_entity(&pool, entity, &slug).await?;
    let directory = tempfile::tempdir()?;
    let memory =
        PostgresMemory::new(pool.clone()).with_wiki_directory(directory.path().to_path_buf());
    let newer_body = session_rewrite(
        newer,
        conversation,
        at("2026-11-07T00:00:00Z"),
        "Newer host.",
        &[("ram", "152 GB available", "2026-11-07T00:30:00Z")],
        "Recorded 152 GB available.",
    );
    let older_body = session_rewrite(
        older,
        conversation,
        at("2026-11-01T00:00:00Z"),
        "Older host.",
        &[
            ("ram", "87 GB available", "2026-11-01T00:30:00Z"),
            ("ip", "11.22.33.44", "2026-11-01T00:30:00Z"),
        ],
        "Recorded the address.",
    );
    apply_once(&memory, entity, &newer_body).await?;
    apply_once(&memory, entity, &older_body).await?;
    assert_ordered(&pool, entity, &slug, newer, directory.path()).await
}

async fn concurrent_case(pool: PgPool) -> TestResult {
    let conversation = seed_conversation(&pool).await?;
    let older = seed_leaf(&pool, conversation, at("2026-11-01T00:00:00Z")).await?;
    let newer = seed_leaf(&pool, conversation, at("2026-11-07T00:00:00Z")).await?;
    let entity = Uuid::new_v4();
    let slug = format!("gpubox-{entity}");
    seed_entity(&pool, entity, &slug).await?;
    let directory = tempfile::tempdir()?;
    let memory =
        PostgresMemory::new(pool.clone()).with_wiki_directory(directory.path().to_path_buf());
    let newer_body = session_rewrite(
        newer,
        conversation,
        at("2026-11-07T00:00:00Z"),
        "Newer host.",
        &[("ram", "152 GB available", "2026-11-07T00:30:00Z")],
        "Recorded 152 GB available.",
    );
    let older_body = session_rewrite(
        older,
        conversation,
        at("2026-11-01T00:00:00Z"),
        "Older host.",
        &[
            ("ram", "87 GB available", "2026-11-01T00:30:00Z"),
            ("ip", "11.22.33.44", "2026-11-01T00:30:00Z"),
        ],
        "Recorded the address.",
    );
    tokio::try_join!(
        apply_once(&memory, entity, &newer_body),
        apply_once(&memory, entity, &older_body),
    )?;
    assert_both_histories(&pool, entity, &slug, newer, directory.path()).await
}

async fn backfill_order(pool: PgPool) -> TestResult {
    let conversation = seed_conversation(&pool).await?;
    let newer = seed_leaf(&pool, conversation, at("2026-11-07T00:00:00Z")).await?;
    let older = seed_leaf(&pool, conversation, at("2026-11-01T00:00:00Z")).await?;
    let middle = seed_leaf(&pool, conversation, at("2026-11-03T00:00:00Z")).await?;
    let memory = PostgresMemory::new(pool);
    let jobs = memory.entity_backfill(BackfillBudget::flat(100)).await?;
    let ids: Vec<Uuid> = jobs.iter().map(|job| job.summary_id).collect();
    let place = |id: Uuid| ids.iter().position(|found| *found == id);
    let older_at = place(older).ok_or("older session missing")?;
    let middle_at = place(middle).ok_or("middle session missing")?;
    let newer_at = place(newer).ok_or("newer session missing")?;
    assert!(older_at < middle_at && middle_at < newer_at);
    let stamped = jobs
        .iter()
        .find(|job| job.summary_id == older)
        .and_then(|job| job.session_at);
    assert_eq!(stamped, Some(at("2026-11-01T00:00:00Z")));
    Ok(())
}

async fn carried_case(pool: PgPool) -> TestResult {
    let scene = scene(pool).await?;
    seed_fact(
        &scene.pool,
        scene.entity,
        "ip",
        "11.22.33.44",
        scene.older,
        scene.conversation,
    )
    .await?;
    seed_fact(
        &scene.pool,
        scene.entity,
        "ram",
        "87 GB available",
        scene.older,
        scene.conversation,
    )
    .await?;
    let page = vec![
        live("ip", "11.22.33.44", "2026-11-01T00:30:00Z"),
        live("ram", "87 GB available", "2026-11-01T00:30:00Z"),
    ];
    let rewrite = page_rewrite(
        "Host.",
        vec![emitted("ram", "87 GB available", "2026-11-07T00:30:00Z")],
        Vec::new(),
        "Kept the address.",
    );
    let body = reconciled_body(
        "Hosts",
        &page,
        &rewrite,
        scene.newer,
        scene.conversation,
        at("2026-11-07T00:00:00Z"),
        "",
    );
    apply_once(&scene.memory, scene.entity, &body).await?;
    let facts = facts_of(&scene.pool, scene.entity).await?;
    let ip = latest(&facts, "ip").ok_or("carried address")?;
    assert_eq!(ip.value, "11.22.33.44");
    assert_eq!(ip.as_of, at("2026-11-01T00:30:00Z"));
    assert_eq!(ip.status, "current");
    Ok(())
}

async fn updated_case(pool: PgPool) -> TestResult {
    let scene = scene(pool).await?;
    seed_fact(
        &scene.pool,
        scene.entity,
        "ram",
        "87 GB available",
        scene.older,
        scene.conversation,
    )
    .await?;
    let page = vec![live("ram", "87 GB available", "2026-11-01T00:30:00Z")];
    let rewrite = page_rewrite(
        "Host.",
        vec![emitted("ram", "152 GB available", "2026-11-07T00:30:00Z")],
        Vec::new(),
        "RAM is 152 GB.",
    );
    let body = reconciled_body(
        "Hosts",
        &page,
        &rewrite,
        scene.newer,
        scene.conversation,
        at("2026-11-07T00:00:00Z"),
        "",
    );
    apply_once(&scene.memory, scene.entity, &body).await?;
    let facts = facts_of(&scene.pool, scene.entity).await?;
    let current = facts
        .iter()
        .filter(|fact| fact.attribute_key == "ram")
        .max_by_key(|fact| (fact.as_of, fact.id))
        .ok_or("current ram")?;
    let previous = facts
        .iter()
        .find(|fact| fact.value == "87 GB available")
        .ok_or("previous ram")?;
    assert_eq!(current.value, "152 GB available");
    assert_ne!(previous.id, current.id);
    let history = history_of(&scene.pool, scene.entity).await?;
    assert_eq!(history, vec!["RAM is 152 GB.".to_owned()]);
    Ok(())
}

async fn retired_case(pool: PgPool) -> TestResult {
    let scene = scene(pool).await?;
    seed_fact(
        &scene.pool,
        scene.entity,
        "ram",
        "152 GB available",
        scene.older,
        scene.conversation,
    )
    .await?;
    seed_fact(
        &scene.pool,
        scene.entity,
        "ip",
        "11.22.33.44",
        scene.older,
        scene.conversation,
    )
    .await?;
    let page = vec![
        live("ram", "152 GB available", "2026-11-01T00:30:00Z"),
        live("ip", "11.22.33.44", "2026-11-01T00:30:00Z"),
    ];
    let rewrite = page_rewrite(
        "Host.",
        vec![emitted("ram", "152 GB available", "2026-11-07T00:30:00Z")],
        vec![RetiredFact {
            attribute: "ip".to_owned(),
            kind: "contradicted".to_owned(),
            detail: "no longer true as of 2026-11-07".to_owned(),
        }],
        "RAM is 152 GB.",
    );
    let body = reconciled_body(
        "Hosts",
        &page,
        &rewrite,
        scene.newer,
        scene.conversation,
        at("2026-11-07T00:00:00Z"),
        "The address is no longer true as of 2026-11-07.",
    );
    apply_once(&scene.memory, scene.entity, &body).await?;
    let facts = facts_of(&scene.pool, scene.entity).await?;
    let ip = latest(&facts, "ip").ok_or("retired address")?;
    assert_eq!(ip.value, "11.22.33.44");
    assert_eq!(ip.status, "current");
    let history = history_of(&scene.pool, scene.entity).await?;
    assert!(
        history
            .iter()
            .any(|line| line.contains("no longer true as of 2026-11-07"))
    );
    Ok(())
}

async fn renamed_case(pool: PgPool) -> TestResult {
    let scene = scene(pool).await?;
    seed_fact(
        &scene.pool,
        scene.entity,
        "other-port",
        "8080",
        scene.older,
        scene.conversation,
    )
    .await?;
    let page = vec![live("other-port", "8080", "2026-11-01T00:30:00Z")];
    let rewrite = page_rewrite(
        "Service.",
        vec![emitted("port", "8080", "2026-11-07T00:30:00Z")],
        vec![RetiredFact {
            attribute: "other-port".to_owned(),
            kind: "renamed".to_owned(),
            detail: "port".to_owned(),
        }],
        "The listen port is 8080.",
    );
    let body = reconciled_body(
        "Services",
        &page,
        &rewrite,
        scene.newer,
        scene.conversation,
        at("2026-11-07T00:00:00Z"),
        "",
    );
    apply_once(&scene.memory, scene.entity, &body).await?;
    let facts = facts_of(&scene.pool, scene.entity).await?;
    let port = latest(&facts, "port").ok_or("renamed port")?;
    assert_eq!(port.value, "8080");
    let old = latest(&facts, "other-port").ok_or("old port stays current")?;
    assert_eq!(old.value, "8080");
    let history = history_of(&scene.pool, scene.entity).await?;
    assert!(history.iter().any(|line| line.contains("renamed to port")));
    Ok(())
}

async fn restated_case(pool: PgPool) -> TestResult {
    let conversation = seed_conversation(&pool).await?;
    let newer = seed_leaf(&pool, conversation, at("2026-11-07T00:00:00Z")).await?;
    let again = seed_leaf(&pool, conversation, at("2026-11-08T00:00:00Z")).await?;
    let later = seed_leaf(&pool, conversation, at("2026-11-09T00:00:00Z")).await?;
    let entity = Uuid::new_v4();
    let slug = format!("restated-{entity}");
    seed_entity(&pool, entity, &slug).await?;
    seed_pinned(&pool, entity, "role", "compute").await?;
    let directory = tempfile::tempdir()?;
    let memory =
        PostgresMemory::new(pool.clone()).with_wiki_directory(directory.path().to_path_buf());
    let first = session_rewrite(
        newer,
        conversation,
        at("2026-11-07T00:00:00Z"),
        "Host.",
        &[
            ("ram", "152 GB available", "2026-11-07T00:30:00Z"),
            ("role", "other", "2026-11-07T00:30:00Z"),
        ],
        "Recorded 152 GB available.",
    );
    apply_once(&memory, entity, &first).await?;
    assert_eq!(
        claim_count(&pool, entity, "ram", "152 GB available").await?,
        1
    );
    assert_eq!(claim_count(&pool, entity, "role", "other").await?, 1);
    assert_eq!(claim_count(&pool, entity, "role", "compute").await?, 1);
    let second = session_rewrite(
        again,
        conversation,
        at("2026-11-08T00:00:00Z"),
        "Host.",
        &[
            ("ram", "152 GB available", "2026-11-08T00:30:00Z"),
            ("role", "compute", "2026-11-08T00:30:00Z"),
        ],
        "Said it again.",
    );
    apply_once(&memory, entity, &second).await?;
    assert_eq!(
        claim_count(&pool, entity, "ram", "152 GB available").await?,
        1_i64
    );
    assert_eq!(claim_count(&pool, entity, "role", "compute").await?, 2);
    let third = session_rewrite(
        later,
        conversation,
        at("2026-11-09T00:00:00Z"),
        "Host.",
        &[("ram", "160 GB available", "2026-11-09T00:30:00Z")],
        "RAM is 160 GB.",
    );
    apply_once(&memory, entity, &third).await?;
    assert_eq!(
        claim_count(&pool, entity, "ram", "160 GB available").await?,
        1
    );
    assert_eq!(
        claim_count(&pool, entity, "ram", "152 GB available").await?,
        1_i64
    );
    let facts = facts_of(&pool, entity).await?;
    assert_eq!(
        latest(&facts, "ram").ok_or("current ram")?.value,
        "160 GB available"
    );
    assert_eq!(
        latest(&facts, "role").ok_or("pinned role")?.value,
        "compute"
    );
    assert_eq!(
        review_items(&memory, entity).await?.as_slice(),
        ["conflict:role"]
    );
    Ok(())
}

async fn claim_count(
    pool: &PgPool,
    entity: Uuid,
    key: &str,
    value: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)
           FROM rh_entity_fact_claims c
           JOIN rh_entity_facts f ON f.id = c.fact_id
          WHERE f.topic_id = $1 AND f.key = $2 AND c.value = $3",
    )
    .bind(entity)
    .bind(key)
    .bind(value)
    .fetch_one(pool)
    .await
}

async fn seed_pinned(
    pool: &PgPool,
    entity: Uuid,
    key: &str,
    value: &str,
) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "WITH fact AS (
            INSERT INTO rh_entity_facts (id, topic_id, key, created_at)
            VALUES ($1, $2, $3, $4)
            RETURNING id
         )
         INSERT INTO rh_entity_fact_claims
            (id, fact_id, value, actor, pinned, at, recorded_at, owner_user_id)
         SELECT $5, fact.id, $6, 'you', true, $4, $4, 'owner'
           FROM fact",
    )
    .bind(Uuid::new_v4())
    .bind(entity)
    .bind(key)
    .bind(at("2026-11-01T00:30:00Z"))
    .bind(Uuid::new_v4())
    .bind(value)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn scene(pool: PgPool) -> Result<Scene, Box<dyn std::error::Error>> {
    let conversation = seed_conversation(&pool).await?;
    let older = seed_leaf(&pool, conversation, at("2026-11-01T00:00:00Z")).await?;
    let newer = seed_leaf(&pool, conversation, at("2026-11-07T00:00:00Z")).await?;
    let entity = Uuid::new_v4();
    let slug = format!("carry-{entity}");
    seed_entity(&pool, entity, &slug).await?;
    let directory = tempfile::tempdir()?;
    let memory =
        PostgresMemory::new(pool.clone()).with_wiki_directory(directory.path().to_path_buf());
    Ok(Scene {
        pool,
        memory,
        entity,
        conversation,
        older,
        newer,
        _directory: directory,
    })
}

struct Scene {
    pool: PgPool,
    memory: PostgresMemory,
    entity: Uuid,
    conversation: Uuid,
    older: Uuid,
    newer: Uuid,
    _directory: tempfile::TempDir,
}

fn live(attribute: &str, value: &str, as_of: &str) -> LiveFact {
    LiveFact {
        attribute: attribute.to_owned(),
        value: value.to_owned(),
        as_of: at(as_of),
        status: FactState::Current,
    }
}

fn emitted(attribute: &str, value: &str, as_of: &str) -> RewriteFact {
    RewriteFact {
        attribute: attribute.to_owned(),
        value: value.to_owned(),
        as_of: as_of.to_owned(),
    }
}

fn page_rewrite(
    summary: &str,
    facts: Vec<RewriteFact>,
    retired: Vec<RetiredFact>,
    sentence: &str,
) -> EntityRewrite {
    EntityRewrite {
        summary: summary.to_owned(),
        article: "Lead.".to_owned(),
        current_facts: facts,
        retired,
        history_sentence: sentence.to_owned(),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the passages are the evidence a contradicted retirement must quote"
)]
fn reconciled_body(
    type_name: &str,
    page: &[LiveFact],
    rewrite: &EntityRewrite,
    summary: Uuid,
    conversation: Uuid,
    session_at: DateTime<Utc>,
    passages: &str,
) -> StoredRewrite {
    let leaves = [LeafClock {
        id: summary,
        conversation_id: Some(conversation),
        start: session_at,
        end: session_at + chrono::Duration::hours(1),
    }];
    let reconciled = reconcile_facts(
        type_name,
        page,
        rewrite,
        summary,
        Some(conversation),
        &leaves,
        passages,
    );
    StoredRewrite {
        summary: rewrite.summary.clone(),
        article: "Lead.".to_owned(),
        facts: reconciled.facts,
        history: session_history(
            summary,
            Some(conversation),
            session_at.date_naive(),
            &reconciled.history_sentence,
        ),
        owner: "owner".to_owned(),
        session_at,
        session_id: summary,
        evidence: passages.to_owned(),
    }
}

async fn history_of(pool: &PgPool, entity: Uuid) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT sentence FROM rh_entity_history WHERE entity_id = $1 ORDER BY session_on, summary_id",
    )
    .bind(entity)
    .fetch_all(pool)
    .await
}

async fn apply_once(
    memory: &PostgresMemory,
    entity: Uuid,
    body: &StoredRewrite,
) -> Result<(), EntityStoreError> {
    let mut lease = memory.lease_entity(entity).await?;
    let applied = lease.apply(body, at("2026-11-08T00:00:00Z")).await;
    let released = lease.release().await;
    applied?;
    released
}

async fn assert_ordered(
    pool: &PgPool,
    entity: Uuid,
    slug: &str,
    newer: Uuid,
    directory: &std::path::Path,
) -> TestResult {
    assert_both_histories(pool, entity, slug, newer, directory).await?;
    let facts = facts_of(pool, entity).await?;
    let ip = facts
        .iter()
        .find(|fact| fact.attribute_key == "ip")
        .ok_or("older address was not added")?;
    assert_eq!(ip.value, "11.22.33.44");
    Ok(())
}

async fn assert_both_histories(
    pool: &PgPool,
    entity: Uuid,
    slug: &str,
    newer: Uuid,
    directory: &std::path::Path,
) -> TestResult {
    let facts = facts_of(pool, entity).await?;
    let ram = facts
        .iter()
        .filter(|fact| fact.attribute_key == "ram")
        .max_by_key(|fact| (fact.as_of, fact.id))
        .ok_or("current ram")?;
    assert_eq!(ram.value, "152 GB available");
    let history = sqlx::query_scalar::<_, String>(
        "SELECT sentence FROM rh_entity_history WHERE entity_id = $1 ORDER BY session_on, summary_id",
    )
    .bind(entity)
    .fetch_all(pool)
    .await?;
    assert_eq!(
        history,
        vec![
            "Recorded the address.".to_owned(),
            "Recorded 152 GB available.".to_owned(),
        ]
    );
    let summary =
        sqlx::query_scalar::<_, String>("SELECT current_state FROM rh_wiki_topics WHERE slug = $1")
            .bind(slug)
            .fetch_one(pool)
            .await?;
    assert_eq!(summary, "Newer host.");
    let page = std::fs::read_to_string(directory.join(format!("topics/{slug}.md")))?;
    let newer_heading = page.find("### 2026-11-07").ok_or("newer history heading")?;
    let older_heading = page.find("### 2026-11-01").ok_or("older history heading")?;
    assert!(newer_heading < older_heading);
    assert!(page.contains("Recorded 152 GB available."));
    assert!(page.contains("Recorded the address."));
    let mark = sqlx::query_as::<_, AppliedSeen>(
        "SELECT applied_through, summary_id FROM rh_entity_applied WHERE entity_id = $1",
    )
    .bind(entity)
    .fetch_one(pool)
    .await?;
    assert_eq!(mark.summary_id, newer);
    assert_eq!(mark.applied_through, at("2026-11-07T00:00:00Z"));
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct AppliedSeen {
    applied_through: DateTime<Utc>,
    summary_id: Uuid,
}

fn session_rewrite(
    summary: Uuid,
    conversation: Uuid,
    session_at: DateTime<Utc>,
    summary_text: &str,
    facts: &[(&str, &str, &str)],
    sentence: &str,
) -> StoredRewrite {
    StoredRewrite {
        summary: summary_text.to_owned(),
        article: "Lead.".to_owned(),
        facts: facts
            .iter()
            .map(|(attribute, value, as_of)| {
                fact(summary, conversation, attribute, value, at(as_of))
            })
            .collect(),
        history: Some(PreparedHistory {
            summary_id: summary,
            conversation_id: Some(conversation),
            session_on: session_at.date_naive(),
            sentence: sentence.to_owned(),
        }),
        owner: "owner".to_owned(),
        session_at,
        session_id: summary,
        evidence: sentence.to_owned(),
    }
}

async fn assert_replaced(
    pool: &PgPool,
    entity: Uuid,
    slug: &str,
    directory: &std::path::Path,
) -> TestResult {
    let facts = facts_of(pool, entity).await?;
    let ram = latest(&facts, "ram").ok_or("current ram")?;
    assert_eq!(ram.value, "160 GB available");
    let ip_row = latest(&facts, "ip").ok_or("ip row")?;
    assert_eq!(ip_row.value, "11.22.33.44");
    assert_ne!(
        latest(&facts, "ram").map(|fact| fact.value.as_str()),
        Some("87 GB available")
    );
    let history = sqlx::query_scalar::<_, String>(
        "SELECT sentence FROM rh_entity_history WHERE entity_id = $1",
    )
    .bind(entity)
    .fetch_all(pool)
    .await?;
    assert_eq!(history, vec!["Recorded 160 GB available.".to_owned()]);
    let article =
        sqlx::query_scalar::<_, String>("SELECT article FROM rh_wiki_topics WHERE slug = $1")
            .bind(slug)
            .fetch_one(pool)
            .await?;
    assert!(article.contains("### Current facts"));
    assert!(article.contains("160 GB available"));
    assert!(!article.contains("87 GB"));
    let page = std::fs::read_to_string(directory.join(format!("topics/{slug}.md")))?;
    assert!(page.contains("Recorded 160 GB available."));
    assert!(page.contains("### Current facts"));
    assert!(!page.contains("87 GB"));
    Ok(())
}

fn rewrite_body(
    summary: Uuid,
    conversation: Uuid,
    session_at: DateTime<Utc>,
    ram: &str,
    sentence: &str,
    keep_gpus: bool,
) -> StoredRewrite {
    let mut facts = vec![fact(
        summary,
        conversation,
        "ram",
        ram,
        at("2026-11-07T00:30:00Z"),
    )];
    if keep_gpus {
        facts.push(fact(
            summary,
            conversation,
            "gpus",
            "8",
            at("2026-11-07T00:30:00Z"),
        ));
    }
    StoredRewrite {
        summary: "GPU host.".to_owned(),
        article: "Lead.".to_owned(),
        facts,
        history: Some(PreparedHistory {
            summary_id: summary,
            conversation_id: Some(conversation),
            session_on: session_at.date_naive(),
            sentence: sentence.to_owned(),
        }),
        owner: "owner".to_owned(),
        session_at,
        session_id: summary,
        evidence: sentence.to_owned(),
    }
}

fn fact(
    summary: Uuid,
    conversation: Uuid,
    attribute: &str,
    value: &str,
    as_of: DateTime<Utc>,
) -> PreparedFact {
    PreparedFact {
        attribute: attribute.to_owned(),
        value: value.to_owned(),
        as_of,
        status: FactState::Current,
        summary_id: summary,
        conversation_id: Some(conversation),
    }
}

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct FactSeen {
    id: Uuid,
    attribute_key: String,
    value: String,
    as_of: DateTime<Utc>,
    status: String,
}

fn latest<'a>(facts: &'a [FactSeen], key: &str) -> Option<&'a FactSeen> {
    facts
        .iter()
        .filter(|fact| fact.attribute_key == key)
        .max_by_key(|fact| (fact.as_of, fact.id))
}

async fn facts_of(pool: &PgPool, entity: Uuid) -> Result<Vec<FactSeen>, sqlx::Error> {
    sqlx::query_as(
        "SELECT c.id, f.key AS attribute_key, c.value, c.at AS as_of, 'current' AS status
           FROM rh_entity_fact_claims c
           JOIN rh_entity_facts f ON f.id = c.fact_id
          WHERE f.topic_id = $1",
    )
    .bind(entity)
    .fetch_all(pool)
    .await
}

async fn seed_conversation(pool: &PgPool) -> Result<Uuid, sqlx::Error> {
    let mut tx = deferred(pool).await?;
    let id = sqlx::query_scalar(
        "INSERT INTO rh_conversations (session_key, agent, channel, title, owner_user_id)
         VALUES ('claude-code:entity-rewrite', 'rivet', 'cli', 'rewrite', 'owner') RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn seed_leaf(
    pool: &PgPool,
    conversation: Uuid,
    start: DateTime<Utc>,
) -> Result<Uuid, sqlx::Error> {
    let mut tx = deferred(pool).await?;
    let id = sqlx::query_scalar(
        "INSERT INTO rh_summaries (conversation_id, content, kind, earliest_at, latest_at, owner_user_id)
         VALUES ($1, 'leaf', 'leaf', $2, $3, 'owner') RETURNING id",
    )
    .bind(conversation)
    .bind(start)
    .bind(start + chrono::Duration::hours(1))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn seed_entity(pool: &PgPool, id: Uuid, slug: &str) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at, owner_user_id)
         VALUES ($1, 'entity', $2, 'gpubox', $3, $3, 'owner')",
    )
    .bind(id)
    .bind(slug)
    .bind(at("2026-11-01T00:00:00Z"))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn seed_fact(
    pool: &PgPool,
    entity: Uuid,
    attribute: &str,
    value: &str,
    summary: Uuid,
    conversation: Uuid,
) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "WITH fact AS (
            INSERT INTO rh_entity_facts (id, topic_id, key, created_at)
            VALUES ($1, $2, $3, $4)
            RETURNING id
         )
         INSERT INTO rh_entity_fact_claims
            (id, fact_id, value, actor, pinned, session_id, summary_id, at, recorded_at, owner_user_id)
         SELECT $5, fact.id, $6, 'wiki-writer', false,
                (SELECT session_key FROM rh_conversations WHERE id = $7),
                $8, $4, $4, 'owner'
           FROM fact",
    )
    .bind(Uuid::new_v4())
    .bind(entity)
    .bind(attribute)
    .bind(at("2026-11-01T00:30:00Z"))
    .bind(Uuid::new_v4())
    .bind(value)
    .bind(conversation)
    .bind(summary)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn deferred(
    pool: &PgPool,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('rivet.defer_embed_enqueue', 'on', true)")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).map_or(DateTime::UNIX_EPOCH, |time| time.with_timezone(&Utc))
}

async fn review_items(
    memory: &PostgresMemory,
    entity: Uuid,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let claims = memory.claims(TopicId::from_uuid(entity)).await?;
    let mut items: Vec<String> = fact_reviews(&claims, Utc::now())
        .into_iter()
        .map(|review| {
            let group = match review.key {
                ReviewKey::Conflict { .. } => "conflict",
                ReviewKey::Fact { .. } => "fact",
                ReviewKey::Filing { .. } | ReviewKey::Tag(_) | ReviewKey::Tidy(_) => "other",
            };
            format!("{group}:{}", review.state.key.as_str())
        })
        .collect();
    items.sort();
    Ok(items)
}
