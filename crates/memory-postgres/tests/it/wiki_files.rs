use kernel::tenancy::Scope;
use kernel::topics::TopicPageStore;
use kernel::wiki::page::WikiSlug;
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
