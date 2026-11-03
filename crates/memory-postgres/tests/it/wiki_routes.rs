use kernel::transcript::SessionLister;
use kernel::wiki::builder::ExtractWiki;
use memory_postgres::{PostgresMemory, PostgresQueue};
use sqlx::PgPool;
use testkit::topic_pages::PageStores;
use testkit::wiki_routes;
use testkit::wiki_routes::RouteStores;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Seeded {
    _directory: tempfile::TempDir,
    memory: PostgresMemory,
    queue: PostgresQueue,
    job: ExtractWiki,
}

impl Seeded {
    async fn new(pool: PgPool) -> TestResult<Self> {
        testkit::install_graphile_worker(&pool).await?;
        let directory = tempfile::tempdir()?;
        let job = summarized(&pool, wiki_routes::SESSION_KEY).await?;
        let memory = PostgresMemory::new(pool).with_wiki_directory(directory.path().join("wiki"));
        let queue = memory.queue().await?;
        Ok(Self {
            _directory: directory,
            memory,
            queue,
            job,
        })
    }

    fn stores(&self) -> RouteStores<'_> {
        self.stores_in(&wiki_routes::NoParents)
    }

    fn stores_in<'a>(&'a self, lineage: &'a dyn SessionLister) -> RouteStores<'a> {
        RouteStores {
            pages: PageStores {
                topics: &self.memory,
                pages: &self.memory,
                decisions: &self.memory,
                wiki: &self.memory,
                queue: &self.queue,
            },
            wiki: &self.memory,
            filing: &self.memory,
            lineage,
        }
    }
}

async fn summarized(pool: &PgPool, key: &str) -> TestResult<ExtractWiki> {
    let conversation = Uuid::new_v4();
    let summary = Uuid::new_v4();
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL rivet.defer_embed_enqueue = 'on'")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO rh_conversations (id, session_key, agent, title, owner_user_id) VALUES ($1, $2, 'rivet', $3, 'owner')",
    )
    .bind(conversation)
    .bind(key)
    .bind(wiki_routes::SESSION_TITLE)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO rh_summaries (id, conversation_id, content, kind, owner_user_id) VALUES ($1, $2, 'AcmeKit notes.', 'leaf', 'owner')",
    )
    .bind(summary)
    .bind(conversation)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ExtractWiki {
        summary_id: summary,
        conversation_id: Some(conversation),
    })
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_placed_session_writes_its_topic_page_and_no_other_topics_page(
    pool: PgPool,
) -> TestResult {
    let seeded = Seeded::new(pool).await?;
    wiki_routes::a_placed_session_writes_its_topic_page_and_no_other_topics_page(
        seeded.stores(),
        &seeded.job,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_topic_page_is_cited_without_a_proposal(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool).await?;
    wiki_routes::the_topic_page_is_cited_without_a_proposal(seeded.stores(), &seeded.job).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_second_session_naming_an_entity_gives_it_its_own_page(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool).await?;
    wiki_routes::a_second_session_naming_an_entity_gives_it_its_own_page(
        seeded.stores(),
        &seeded.job,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_entity_page_names_the_sessions_that_cite_it(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool).await?;
    testkit::wiki_mentions::an_entity_page_names_the_sessions_that_cite_it(
        seeded.stores(),
        &seeded.job,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_mention_held_before_its_entity_existed_still_lands(pool: PgPool) -> TestResult {
    let earlier = summarized(&pool, testkit::wiki_mentions::EARLIER_KEY).await?;
    let seeded = Seeded::new(pool).await?;
    testkit::wiki_mentions::a_mention_held_before_its_entity_existed_still_lands(
        seeded.stores(),
        &seeded.job,
        &earlier,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_entity_type_is_found_by_its_whole_path(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool).await?;
    testkit::wiki_mentions::an_entity_type_is_found_by_its_whole_path(seeded.stores(), &seeded.job)
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_entity_goes_by_its_names_never_by_a_description(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool).await?;
    testkit::wiki_mentions::an_entity_goes_by_its_names_never_by_a_description(
        seeded.stores(),
        &seeded.job,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_unfiled_session_waits_until_it_is_filed(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool).await?;
    wiki_routes::an_unfiled_session_waits_until_it_is_filed(seeded.stores(), &seeded.job).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_child_session_waits_for_its_parent(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool).await?;
    wiki_routes::a_child_session_waits_for_its_parent(
        seeded.stores_in(&testkit::wiki_lineage::CHILD),
        &seeded.job,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_nested_child_s_notes_land_on_its_filed_ancestor_s_topic(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool).await?;
    testkit::wiki_lineage::a_child_s_notes_land_on_its_filed_ancestor_s_topic(
        seeded.stores_in(&testkit::wiki_lineage::GRANDCHILD),
        &seeded.job,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_subagent_key_s_notes_land_on_its_parent_s_topic(pool: PgPool) -> TestResult {
    let subagent = summarized(&pool, testkit::wiki_lineage::SUBAGENT_KEY).await?;
    let seeded = Seeded::new(pool).await?;
    testkit::wiki_lineage::a_child_s_notes_land_on_its_filed_ancestor_s_topic(
        seeded.stores(),
        &subagent,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_moved_session_takes_its_notes_along(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool).await?;
    wiki_routes::a_moved_session_takes_its_notes_along(seeded.stores(), &seeded.job).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_deleted_entity_is_not_recreated(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool).await?;
    wiki_routes::a_deleted_entity_is_not_recreated(seeded.stores(), &seeded.job).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_sub_topic_summary_lands_in_its_section(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool).await?;
    wiki_routes::a_sub_topic_summary_lands_in_its_section(seeded.stores(), &seeded.job).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_sub_topic_rewrite_keeps_its_lead(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool.clone()).await?;
    let second = another(&pool, &seeded.job).await?;
    testkit::wiki_bounded::a_sub_topic_rewrite_keeps_its_lead(seeded.stores(), &seeded.job, &second)
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn concurrent_rewrites_of_one_page_keep_both(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool.clone()).await?;
    let second = another(&pool, &seeded.job).await?;
    testkit::wiki_bounded::concurrent_rewrites_of_one_page_keep_both(
        seeded.stores(),
        &seeded.job,
        &second,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_entity_mention_is_merged_into_its_page(pool: PgPool) -> TestResult {
    let second = summarized(&pool, testkit::wiki_mentions::EARLIER_KEY).await?;
    let seeded = Seeded::new(pool).await?;
    testkit::wiki_bounded::an_entity_mention_is_merged_into_its_page(
        seeded.stores(),
        &seeded.job,
        &second,
    )
    .await
}

async fn another(pool: &PgPool, job: &ExtractWiki) -> TestResult<ExtractWiki> {
    let summary = Uuid::new_v4();
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL rivet.defer_embed_enqueue = 'on'")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO rh_summaries (id, conversation_id, content, kind, owner_user_id) VALUES ($1, $2, 'More AcmeKit notes.', 'leaf', 'owner')",
    )
    .bind(summary)
    .bind(job.conversation_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ExtractWiki {
        summary_id: summary,
        conversation_id: job.conversation_id,
    })
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn updates_keep_the_lead_and_replace_only_the_currently_sentence(pool: PgPool) -> TestResult {
    let seeded = Seeded::new(pool.clone()).await?;
    let second = another(&pool, &seeded.job).await?;
    let third = another(&pool, &seeded.job).await?;
    testkit::wiki_bounded::updates_keep_the_lead_and_replace_only_the_currently_sentence(
        seeded.stores(),
        [&seeded.job, &second, &third],
    )
    .await
}
