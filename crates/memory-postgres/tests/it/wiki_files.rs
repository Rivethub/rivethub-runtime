use kernel::tenancy::Scope;
use kernel::topics::NewTopic;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use kernel::topics::TopicPage;
use kernel::topics::TopicPageStore;
use kernel::topics::TopicStore as _;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::bound::ARTICLE_BUDGET;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use testkit::wiki_files::CONVERSATION;
use testkit::wiki_files::same_commits;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn page_files_match_sqlite(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    sqlx::query(
        "INSERT INTO rh_conversations (id, session_key, agent, title, owner_user_id)
         VALUES ($1, 'claude-code:page-files', 'rivet', 'page files', 'owner')",
    )
    .bind(uuid::Uuid::parse_str(CONVERSATION)?)
    .execute(&pool)
    .await?;
    let root = std::fs::canonicalize(wiki.path())?;
    let store = PostgresMemory::new(pool).with_wiki_directory(root.clone());
    Scope::Maintenance
        .run(async {
            same_commits(&store, &root).await?;
            testkit::wiki_files::commit_failure_keeps_rows(&store, &root, || {
                wiki_files::wiki_writer::fail_git(&root, "commit")
            })
            .await?;
            testkit::wiki_files::stale_publish_keeps_the_file(&store, &root).await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_late_sweep_keeps_the_entity_page(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    sqlx::query(
        "INSERT INTO rh_conversations (id, session_key, agent, title, owner_user_id)
         VALUES ($1, 'claude-code:page-race', 'rivet', 'page race', 'owner')",
    )
    .bind(uuid::Uuid::parse_str(CONVERSATION)?)
    .execute(&pool)
    .await?;
    let root = std::fs::canonicalize(wiki.path())?;
    let store = PostgresMemory::new(pool).with_wiki_directory(root.clone());
    Scope::Maintenance
        .run(async { testkit::wiki_files::a_late_sweep_keeps_the_entity_page(&store, &root).await })
        .await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_numbered_long_slug_keeps_the_page_it_shortens_to(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = std::fs::canonicalize(wiki.path())?;
    let store = PostgresMemory::new(pool).with_wiki_directory(root.clone());
    Scope::Maintenance
        .run(testkit::wiki_files::a_numbered_long_slug_keeps_the_page_it_shortens_to(&store, &root))
        .await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_sweep_keeps_a_page_its_pull_brings_in(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = std::fs::canonicalize(wiki.path())?;
    let store = PostgresMemory::new(pool).with_wiki_directory(root.clone());
    Scope::Maintenance
        .run(testkit::wiki_files::a_sweep_keeps_a_page_its_pull_brings_in(&store, &root))
        .await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_sweep_skips_a_page_file_without_a_row(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = std::fs::canonicalize(wiki.path())?;
    let store = PostgresMemory::new(pool).with_wiki_directory(root.clone());
    Scope::Maintenance
        .run(testkit::wiki_files::a_sweep_skips_a_page_file_without_a_row(&store, &root))
        .await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_retitled_page_with_an_oversized_article_is_stored_bounded(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = std::fs::canonicalize(wiki.path())?;
    let store = PostgresMemory::new(pool.clone()).with_wiki_directory(root);
    let id = TopicId::from_uuid(uuid::Uuid::parse_str(
        "00000000-0000-4000-8000-0000000000f7",
    )?);
    let now: jiff::Timestamp = "2026-10-10T12:00:00Z".parse()?;
    let page = |title: &str, article: String| -> Result<TopicPage, Box<dyn std::error::Error>> {
        Ok(TopicPage {
            slug: WikiSlug::try_from("field-notes".to_owned())?,
            title: title.to_owned(),
            summary: "Notes from the bench.".to_owned(),
            article,
            sections: Vec::new(),
        })
    };
    let oversized = (0..1000)
        .map(|fact| {
            let words: Vec<String> = (0..25)
                .map(|word| format!("f{fact}w{word}{}", "q".repeat(40)))
                .collect();
            format!("Fact {fact}: {}", words.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    Scope::Maintenance
        .run(async {
            store
                .create(Topic::create(
                    id,
                    NewTopic {
                        kind: TopicKind::Work,
                        name: TopicName::new("Field notes")?,
                        aliases: Vec::new(),
                        parent: None,
                        directory: None,
                        owner: Some(testkit::owner()?),
                    },
                    now,
                )?)
                .await?;
            store
                .save_topic_page(
                    id,
                    &page("Field notes", "The bench faces the window.".to_owned())?,
                    now,
                )
                .await?;
            store
                .save_topic_page(id, &page("Bench notes", oversized)?, now)
                .await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await?;
    let (title, article): (String, String) =
        sqlx::query_as("SELECT title, article FROM rh_wiki_topics WHERE slug = 'field-notes'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(title, "Bench notes");
    assert!(
        article.len() <= ARTICLE_BUDGET,
        "row article is {} bytes",
        article.len()
    );
    assert!(article.contains("Fact 0:"));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn removing_a_missing_file_in_a_new_repository_deletes_the_row(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = std::fs::canonicalize(wiki.path())?;
    sqlx::query("INSERT INTO rh_wiki_topics (slug, title) VALUES ('unborn', 'Unborn')")
        .execute(&pool)
        .await?;
    let store = PostgresMemory::new(pool.clone()).with_wiki_directory(root);
    let slug = WikiSlug::try_from("unborn".to_owned())?;
    Scope::Maintenance
        .run(async move {
            store.remove_page(&slug).await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await?;
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_wiki_topics WHERE slug = 'unborn'")
        .fetch_one(&pool)
        .await?;
    assert_eq!(left, 0);
    Ok(())
}
