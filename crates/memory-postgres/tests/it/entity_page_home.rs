use jiff::SignedDuration;
use jiff::Timestamp;
use kernel::owner::Owner;
use kernel::tenancy::Scope;
use kernel::tenancy::Viewer;
use kernel::topics::SweepOwners;
use kernel::topics::SyncTopicPage;
use kernel::topics::TopicId;
use kernel::topics::TopicPageStore as _;
use kernel::topics::sync_topic_page;
use kernel::wiki::entity_pass::EntityStore as _;
use kernel::wiki::entity_pass::PreparedHistory;
use kernel::wiki::entity_pass::StoredRewrite;
use kernel::wiki::page::WikiSlug;
use memory_postgres::PostgresMemory;
use memory_postgres::time::PgTimestamp;
use sqlx::PgPool;
use uuid::Uuid;
use wire::time::TimestampFormat as _;

use crate::common;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct PageRow {
    slug: String,
    owner_user_id: Option<String>,
    scope: Option<String>,
    local_slug: Option<String>,
}

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct StoredPage {
    slug: String,
    owner_user_id: Option<String>,
    current_state: String,
    topic_id: Option<Uuid>,
}

struct SeededPage<'a> {
    key: &'a str,
    local: &'a str,
    owner: &'a str,
    topic: Option<Uuid>,
    state: &'a str,
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_pages_in_a_shared_store_live_in_their_owners_wiki(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = PostgresMemory::new(common::probe(&pool).await?)
        .with_wiki_directory(directory.path().to_path_buf())
        .with_wiki_owner(Owner::new("phil".to_owned())?);
    let mut seeded = Vec::new();
    for (owner, home, key) in [
        ("phil", "", "philbox"),
        ("coco", "users/coco", "coco-cocobox"),
    ] {
        let entity = Uuid::new_v4();
        let slug = format!("{owner}box");
        let (conversation, leaf) = seed_session(&pool, owner).await?;
        seed_entity(&pool, entity, &slug, owner).await?;
        seeded.push((
            owner,
            home,
            key,
            entity,
            slug,
            rewrite(leaf, conversation, owner),
        ));
    }
    for (owner, home, key, entity, slug, body) in seeded {
        apply(&memory, entity, &body).await?;

        let row = sqlx::query_as::<_, PageRow>(
            "SELECT slug, owner_user_id, scope, local_slug FROM rh_wiki_topics WHERE topic_id = $1",
        )
        .bind(entity)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            row,
            PageRow {
                slug: key.to_owned(),
                owner_user_id: Some(owner.to_owned()),
                scope: Some(owner.to_owned()),
                local_slug: Some(slug.clone()),
            }
        );
        let file = directory
            .path()
            .join(home)
            .join(format!("topics/{slug}.md"));
        assert!(file.exists(), "{owner}'s page file is in {home:?}");

        let viewer = Scope::Viewer(Viewer::new(Owner::new(owner.to_owned())?, Vec::new()));
        let you = Owner::new(owner.to_owned())?;
        viewer
            .clone()
            .run(sync_topic_page(
                &memory,
                &memory,
                &memory,
                &SyncTopicPage::topic(TopicId::from_uuid(entity)),
                SweepOwners::All,
                at("2026-11-07T02:00:00Z"),
            ))
            .await?;
        let failures = viewer
            .clone()
            .run(sync_topic_page(
                &memory,
                &memory,
                &memory,
                &SyncTopicPage::every_topic(),
                SweepOwners::Only(Some(&you)),
                at("2026-11-07T02:00:00Z"),
            ))
            .await?;
        assert!(failures.is_empty(), "{owner}'s sweep failed: {failures:?}");
        let page = viewer
            .run(memory.topic_page(&WikiSlug::try_from(key.to_owned())?))
            .await?
            .ok_or("the owner reads the entity page")?;
        assert_eq!(page.summary, "GPU host.");
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_entity_keeps_writing_to_the_numbered_page_it_is_bound_to(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = shared_memory(&pool, directory.path()).await?;
    let entity = Uuid::new_v4();
    let (conversation, leaf) = seed_session(&pool, "phil").await?;
    seed_entity(&pool, entity, "phil-memory", "phil").await?;
    seed_page(
        &pool,
        &SeededPage {
            key: "phil-memory",
            local: "phil-memory",
            owner: "phil",
            topic: None,
            state: "Work notes.",
        },
    )
    .await?;
    seed_page(
        &pool,
        &SeededPage {
            key: "phil-memory-2",
            local: "phil-memory-2",
            owner: "phil",
            topic: Some(entity),
            state: "Old summary.",
        },
    )
    .await?;

    apply(&memory, entity, &rewrite(leaf, conversation, "phil")).await?;

    assert_eq!(
        pages(&pool).await?,
        vec![
            stored("phil-memory", "phil", "Work notes.", None),
            stored("phil-memory-2", "phil", "GPU host.", Some(entity)),
        ]
    );
    assert!(directory.path().join("topics/phil-memory-2.md").exists());
    assert!(!directory.path().join("topics/phil-memory.md").exists());
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_entity_named_like_another_owners_page_leaves_that_page_alone(
    pool: PgPool,
) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = shared_memory(&pool, directory.path()).await?;
    let entity = Uuid::new_v4();
    let (conversation, leaf) = seed_session(&pool, "coco").await?;
    seed_entity(&pool, entity, "phil", "coco").await?;
    seed_page(
        &pool,
        &SeededPage {
            key: "phil",
            local: "phil",
            owner: "phil",
            topic: None,
            state: "Phil's own page.",
        },
    )
    .await?;

    apply(&memory, entity, &rewrite(leaf, conversation, "coco")).await?;

    assert_eq!(
        pages(&pool).await?,
        vec![
            stored("coco-phil", "coco", "GPU host.", Some(entity)),
            stored("phil", "phil", "Phil's own page.", None),
        ]
    );
    let row = sqlx::query_as::<_, PageRow>(
        "SELECT slug, owner_user_id, scope, local_slug FROM rh_wiki_topics WHERE topic_id = $1",
    )
    .bind(entity)
    .fetch_one(&pool)
    .await?;
    assert_eq!(row.scope.as_deref(), Some("coco"));
    assert_eq!(row.local_slug.as_deref(), Some("phil"));
    assert!(directory.path().join("users/coco/topics/phil.md").exists());
    assert!(!directory.path().join("topics/phil.md").exists());
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_merge_leaves_another_owners_pages_alone(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = shared_memory(&pool, directory.path()).await?;
    let mut ids = [Uuid::new_v4(), Uuid::new_v4()];
    ids.sort_unstable();
    let [survivor, absorbed] = ids;
    seed_entity(&pool, survivor, "box_a", "coco").await?;
    seed_entity(&pool, absorbed, "g9", "coco").await?;
    for (key, local, owner, topic, state) in [
        ("box_a", "box_a", "phil", None, "Phil's box."),
        ("g9", "g9", "phil", None, "Phil's g9."),
        ("coco-box_a", "box_a", "coco", Some(survivor), "Coco's box."),
        ("coco-g9", "g9", "coco", Some(absorbed), "Coco's g9."),
    ] {
        seed_page(
            &pool,
            &SeededPage {
                key,
                local,
                owner,
                topic,
                state,
            },
        )
        .await?;
    }

    let merged = Scope::Maintenance
        .run(memory.merge_entities(absorbed, survivor, at("2026-11-12T00:00:00Z")))
        .await?
        .ok_or("merge returned nothing")?;

    assert_eq!(merged.survivor, survivor);
    assert_eq!(
        pages(&pool).await?,
        vec![
            stored("box_a", "phil", "Phil's box.", None),
            stored("coco-box_a", "coco", "Coco's box.", Some(survivor)),
            stored("g9", "phil", "Phil's g9.", None),
        ]
    );
    let redirects: Vec<(String, String)> =
        sqlx::query_as("SELECT from_slug, to_slug FROM rh_wiki_redirects ORDER BY from_slug")
            .fetch_all(&pool)
            .await?;
    assert_eq!(
        redirects,
        vec![("coco-g9".to_owned(), "coco-box_a".to_owned())]
    );
    Ok(())
}

async fn shared_memory(
    pool: &PgPool,
    directory: &std::path::Path,
) -> Result<PostgresMemory, Box<dyn std::error::Error>> {
    for (scope, prefix, home) in [("phil", "", ""), ("coco", "coco-", "users/coco")] {
        sqlx::query(
            "INSERT INTO rh_wiki_scopes (scope, owner_user_id, visibility, key_prefix, directory)
             VALUES ($1, $1, 'private', $2, $3)",
        )
        .bind(scope)
        .bind(prefix)
        .bind(home)
        .execute(pool)
        .await?;
    }
    Ok(PostgresMemory::new(common::probe(pool).await?)
        .with_wiki_directory(directory.to_path_buf())
        .with_wiki_owner(Owner::new("phil".to_owned())?))
}

async fn apply(
    memory: &PostgresMemory,
    entity: Uuid,
    body: &StoredRewrite,
) -> Result<(), kernel::wiki::entity_pass::EntityStoreError> {
    Scope::Maintenance
        .run(async {
            let mut lease = memory.lease_entity(entity).await?;
            let applied = lease.apply(body, at("2026-11-07T01:00:00Z")).await;
            lease.release().await?;
            applied.map(drop)
        })
        .await
}

async fn seed_page(pool: &PgPool, page: &SeededPage<'_>) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('rivet.defer_embed_enqueue', 'on', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO rh_wiki_topics
            (slug, title, current_state, topic_id, owner_user_id, scope, local_slug, visibility)
         VALUES ($1, $2, $3, $4, $5, $5, $2, 'private')",
    )
    .bind(page.key)
    .bind(page.local)
    .bind(page.state)
    .bind(page.topic)
    .bind(page.owner)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

async fn pages(pool: &PgPool) -> Result<Vec<StoredPage>, sqlx::Error> {
    sqlx::query_as::<_, StoredPage>(
        "SELECT slug, owner_user_id, current_state, topic_id FROM rh_wiki_topics ORDER BY slug",
    )
    .fetch_all(pool)
    .await
}

fn stored(slug: &str, owner: &str, state: &str, topic: Option<Uuid>) -> StoredPage {
    StoredPage {
        slug: slug.to_owned(),
        owner_user_id: Some(owner.to_owned()),
        current_state: state.to_owned(),
        topic_id: topic,
    }
}

async fn seed_session(pool: &PgPool, owner: &str) -> Result<(Uuid, Uuid), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('rivet.defer_embed_enqueue', 'on', true)")
        .execute(&mut *tx)
        .await?;
    let conversation: Uuid = sqlx::query_scalar(
        "INSERT INTO rh_conversations (session_key, agent, channel, title, owner_user_id)
         VALUES ($1, 'rivet', 'cli', 'entity home', $2) RETURNING id",
    )
    .bind(format!("claude-code:entity-home-{owner}"))
    .bind(owner)
    .fetch_one(&mut *tx)
    .await?;
    let start = at("2026-11-07T00:00:00Z");
    let leaf = sqlx::query_scalar(
        "INSERT INTO rh_summaries (conversation_id, content, kind, earliest_at, latest_at, owner_user_id)
         VALUES ($1, 'leaf', 'leaf', $2, $3, $4) RETURNING id",
    )
    .bind(conversation)
    .bind(PgTimestamp::from(start))
    .bind(PgTimestamp::from(start + SignedDuration::from_hours(1)))
    .bind(owner)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((conversation, leaf))
}

async fn seed_entity(pool: &PgPool, id: Uuid, slug: &str, owner: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at, owner_user_id)
         VALUES ($1, 'entity', $2, $2, $3, $3, $4)",
    )
    .bind(id)
    .bind(slug)
    .bind(PgTimestamp::from(at("2026-11-01T00:00:00Z")))
    .bind(owner)
    .execute(pool)
    .await
    .map(drop)
}

fn rewrite(leaf: Uuid, conversation: Uuid, owner: &str) -> StoredRewrite {
    let session_at = at("2026-11-07T00:00:00Z");
    StoredRewrite {
        summary: "GPU host.".to_owned(),
        article: "Lead.".to_owned(),
        facts: Vec::new(),
        history: Some(PreparedHistory {
            summary_id: leaf,
            conversation_id: Some(conversation),
            session_on: session_at.utc_date(),
            sentence: "Seen.".to_owned(),
        }),
        owner: owner.to_owned(),
        session_at,
        session_id: leaf,
        evidence: "Seen.".to_owned(),
    }
}

fn at(text: &str) -> Timestamp {
    wire::time::parse_rfc3339(text).unwrap_or(Timestamp::UNIX_EPOCH)
}
