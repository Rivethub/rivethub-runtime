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
    owner_user_id: Option<String>,
    scope: Option<String>,
    local_slug: Option<String>,
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_pages_in_a_shared_store_live_in_their_owners_wiki(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = PostgresMemory::new(common::probe(&pool).await?)
        .with_wiki_directory(directory.path().to_path_buf())
        .with_wiki_owner(Owner::new("phil".to_owned())?);
    let mut seeded = Vec::new();
    for (owner, home) in [("phil", ""), ("coco", "users/coco")] {
        let entity = Uuid::new_v4();
        let slug = format!("{owner}box");
        let (conversation, leaf) = seed_session(&pool, owner).await?;
        seed_entity(&pool, entity, &slug, owner).await?;
        seeded.push((
            owner,
            home,
            entity,
            slug,
            rewrite(leaf, conversation, owner),
        ));
    }
    for (owner, home, entity, slug, body) in seeded {
        Scope::Maintenance
            .run(async {
                let mut lease = memory.lease_entity(entity).await?;
                let applied = lease.apply(&body, at("2026-11-07T01:00:00Z")).await;
                lease.release().await?;
                applied.map(drop)
            })
            .await?;

        let row = sqlx::query_as::<_, PageRow>(
            "SELECT owner_user_id, scope, local_slug FROM rh_wiki_topics WHERE topic_id = $1",
        )
        .bind(entity)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            row,
            PageRow {
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
            .run(memory.topic_page(&WikiSlug::try_from(slug.clone())?))
            .await?
            .ok_or("the owner reads the entity page")?;
        assert_eq!(page.summary, "GPU host.");
    }
    Ok(())
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
