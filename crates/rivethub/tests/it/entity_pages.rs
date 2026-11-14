use std::sync::Arc;
use std::time::Duration;

use chrono::DateTime;
use chrono::Utc;
use gateway::Gateway;
use kernel::tenancy::Scope;
use kernel::topics::TopicId;
use kernel::topics::TopicStore;
use kernel::topics::bound_page;
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityStore;
use kernel::wiki::entity_pass::PreparedEntity;
use kernel::wiki::entity_pass::PreparedFact;
use kernel::wiki::entity_pass::PreparedHistory;
use kernel::wiki::entity_pass::StoredRewrite;
use memory_postgres::PostgresMemory;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::gateway::entities::ChangeKind;
use wire::gateway::entities::EntityArticle;
use wire::gateway::entities::EntitySummary;
use wire::gateway::entities::EntityTypeInfo;
use wire::gateway::topics::TopicPage;
use wire::session_id::SessionId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SESSION: &str = "claude-code:entity-serve";

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario covering the committed page, the topic page, the entity article and the list"
)]
async fn a_committed_entity_page_is_served_with_its_type_and_history() -> TestResult {
    let db = crate::pgdb::TestDb::migrated().await?;
    let pool = db.pool();
    let entity = Uuid::new_v4();
    let slug = format!("gpubox-{entity}");
    seed(&pool, entity, &slug).await?;
    let home = tempfile::tempdir()?;
    let memory = PostgresMemory::new(pool.clone()).with_wiki_directory(home.path().to_path_buf());
    Scope::Maintenance.run(apply(&memory, entity)).await?;
    Scope::Maintenance.run(file_fleet(&memory)).await?;
    let topic = TopicStore::list(&memory)
        .await?
        .into_iter()
        .find(|listing| listing.topic.id == TopicId::from_uuid(entity))
        .map(|listing| listing.topic)
        .ok_or("entity topic")?;
    let page = bound_page(&memory, &memory, &topic)
        .await?
        .ok_or("bound page")?;
    assert_eq!(page.current_state, "GPU host.");
    let memory = Arc::new(memory);
    let tenancy = Arc::new(memory_postgres::PostgresTenancy::new(pool.clone()));
    let gateway = Arc::new(
        Gateway::new(
            memory.clone(),
            gateway::Settings {
                hostname: "entity-pages".to_owned(),
                node: "entity-pages".to_owned(),
                allowed_origins: Vec::new(),
                allowed_hosts: Vec::new(),
                static_dir: None,
                root_redirect: None,
                state_dir: home.path().join("state"),
                private_ca: None,
                request_deadline: gateway::REQUEST_DEADLINE,
                policy: None,
            },
        )
        .with_topics(memory.clone(), memory.clone())
        .with_facts(memory)
        .with_tenancy_store(tenancy)
        .with_local_owner(crate::owner::holder())
        .with_tenancy(gateway::tenancy::Tenancy::local(
            gateway::tenancy::UserId::new("owner".to_owned())?,
        )),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let shutdown = CancellationToken::new();
    let shutdown_guard = shutdown.clone().drop_guard();
    let mut servers = JoinSet::new();
    servers.spawn(gateway::server::serve(
        listener,
        gateway,
        None,
        shutdown.clone(),
    ));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .default_headers(crate::owner::holder_headers()?)
        .build()?;
    let topic_page: TopicPage = client
        .get(format!("{base}/api/topics/{entity}/page"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let served = topic_page.page.ok_or("topic page")?;
    assert_eq!(served.slug, slug);
    assert_eq!(served.current_state, "GPU host.");
    let article: EntityArticle = client
        .get(format!("{base}/api/entities/{entity}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(article.lead, "GPU host.");
    assert_eq!(article.summary.entity_type, "hosts");
    assert_eq!(article.summary.type_name, "Hosts");
    let history = article
        .timeline
        .iter()
        .find(|entry| entry.kind == ChangeKind::History)
        .ok_or("history entry")?;
    assert_eq!(history.text, "Recorded the address.");
    let source = history.source.as_ref().ok_or("history session")?;
    assert_eq!(source.id, SESSION.parse::<SessionId>()?);
    assert!(
        article
            .timeline
            .iter()
            .any(|entry| entry.kind == ChangeKind::Fact)
    );
    let listed: Vec<EntitySummary> = client
        .get(format!("{base}/api/entities"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(kind_of(&listed, "Acme").as_deref(), Some("organizations"));
    assert_eq!(kind_of(&listed, "RivetHub").as_deref(), Some("projects"));
    assert_eq!(kind_of(&listed, "Office").as_deref(), Some("places"));
    assert_eq!(kind_of(&listed, "gpubox").as_deref(), Some("hosts"));
    assert_eq!(kind_of(&listed, "north").as_deref(), Some("fleets"));
    let north = listed
        .iter()
        .find(|entity| entity.title == "north")
        .ok_or("north")?;
    assert_eq!(north.type_name, "Fleets");
    let types: Vec<EntityTypeInfo> = client
        .get(format!("{base}/api/entities/types"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let fleets = types
        .iter()
        .find(|row| row.key == "fleets")
        .ok_or("fleets type")?;
    assert_eq!(fleets.type_name, "Fleets");
    assert_eq!(fleets.count, 1);
    drop(shutdown_guard);
    Ok(())
}

fn kind_of(listed: &[EntitySummary], title: &str) -> Option<String> {
    listed
        .iter()
        .find(|entity| entity.title == title)
        .map(|entity| entity.entity_type.clone())
}

async fn file_fleet(memory: &PostgresMemory) -> TestResult {
    let prepared = PreparedEntity {
        entity_ref: None,
        name: "north".to_owned(),
        type_name: "Fleets".to_owned(),
        aliases: Vec::new(),
        facts: Vec::new(),
        history: Vec::new(),
    };
    let leaf = EntityLeaf {
        id: Uuid::new_v4(),
        conversation_id: None,
        content: "The north fleet is based at the yard.".to_owned(),
        start: at("2026-11-07T00:00:00Z"),
        end: at("2026-11-07T01:00:00Z"),
        owner: Some("owner".to_owned()),
    };
    memory
        .commit(
            std::slice::from_ref(&leaf),
            &prepared,
            at("2026-11-07T02:00:00Z"),
        )
        .await?
        .ok_or("fleet was not filed")?;
    Ok(())
}

async fn apply(memory: &PostgresMemory, entity: Uuid) -> TestResult {
    let conversation = seed_conversation(memory).await?;
    let summary = seed_summary(memory, conversation).await?;
    let body = StoredRewrite {
        summary: "GPU host.".to_owned(),
        article: "Lead.".to_owned(),
        facts: vec![PreparedFact {
            attribute: "ram".to_owned(),
            value: "152 GB available".to_owned(),
            as_of: at("2026-11-07T00:30:00Z"),
            status: kernel::wiki::entity_pass::FactState::Current,
            summary_id: summary,
            conversation_id: Some(conversation),
        }],
        history: Some(PreparedHistory {
            summary_id: summary,
            conversation_id: Some(conversation),
            session_on: at("2026-11-07T00:00:00Z").date_naive(),
            sentence: "Recorded the address.".to_owned(),
        }),
        owner: "owner".to_owned(),
        session_at: at("2026-11-07T00:00:00Z"),
        session_id: summary,
        evidence: "Recorded the address.".to_owned(),
    };
    let mut lease = memory.lease_entity(entity).await?;
    lease.apply(&body, at("2026-11-07T01:00:00Z")).await?;
    lease.release().await?;
    Ok(())
}

async fn seed(pool: &sqlx::PgPool, entity: Uuid, slug: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_topics (id, kind, slug, name, parent_id, created_at, updated_at, owner_user_id)
         SELECT $1, 'entity', $2, 'gpubox', parent.id, $3, $3, 'owner'
           FROM rh_topics parent
          WHERE parent.kind = 'type' AND parent.slug = 'hosts'",
    )
    .bind(entity)
    .bind(slug)
    .bind(at("2026-11-01T00:00:00Z"))
    .execute(pool)
    .await?;
    for (parent, slug, name) in [
        ("organizations", "acme", "Acme"),
        ("projects", "rivethub", "RivetHub"),
        ("places", "office", "Office"),
    ] {
        sqlx::query(
            "INSERT INTO rh_topics (id, kind, slug, name, parent_id, created_at, updated_at, owner_user_id)
             SELECT gen_random_uuid(), 'entity', $1, $2, parent.id, now(), now(), 'owner'
               FROM rh_topics parent
              WHERE parent.kind = 'type' AND parent.slug = $3",
        )
        .bind(slug)
        .bind(name)
        .bind(parent)
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn seed_conversation(memory: &PostgresMemory) -> Result<Uuid, sqlx::Error> {
    let pool = pool_of(memory);
    sqlx::query_scalar(
        "INSERT INTO rh_conversations (session_key, agent, channel, title, owner_user_id)
         VALUES ($1, 'rivet', 'cli', 'serve', 'owner') RETURNING id",
    )
    .bind(SESSION)
    .fetch_one(pool)
    .await
}

async fn seed_summary(memory: &PostgresMemory, conversation: Uuid) -> Result<Uuid, sqlx::Error> {
    let pool = pool_of(memory);
    let start = at("2026-11-07T00:00:00Z");
    sqlx::query_scalar(
        "INSERT INTO rh_summaries (conversation_id, content, kind, earliest_at, latest_at, owner_user_id)
         VALUES ($1, 'leaf', 'leaf', $2, $3, 'owner') RETURNING id",
    )
    .bind(conversation)
    .bind(start)
    .bind(start + chrono::Duration::hours(1))
    .fetch_one(pool)
    .await
}

fn pool_of(memory: &PostgresMemory) -> &sqlx::PgPool {
    memory.pool()
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).map_or(DateTime::UNIX_EPOCH, |time| time.with_timezone(&Utc))
}
