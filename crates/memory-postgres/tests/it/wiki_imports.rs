use kernel::memory::MemoryStore as _;
use kernel::memory::Verification;
use kernel::wiki::WikiLookup;
use kernel::wiki::page::WikiSlug;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const IMPORT_KEY: &str = "import:9f2c0000000000000000000000000000000000000000000000000000000000e1";

async fn cite(pool: &PgPool, key: &str, settings: &str, slug: &str) -> Result<Uuid, sqlx::Error> {
    let conversation: Uuid = sqlx::query_scalar(
        "INSERT INTO rh_conversations (session_key, agent, channel, settings, owner_user_id)
         VALUES ($1, 'opus', 'claude-code', $2::jsonb, 'owner') RETURNING id",
    )
    .bind(key)
    .bind(settings)
    .fetch_one(pool)
    .await?;
    let summary: Uuid = sqlx::query_scalar(
        "INSERT INTO rh_summaries (conversation_id, content, owner_user_id)
         VALUES ($1, 'text', 'owner') RETURNING id",
    )
    .bind(conversation)
    .fetch_one(pool)
    .await?;
    sqlx::query(
        "INSERT INTO rh_wiki_citations (topic_slug, summary_id, owner_user_id)
         VALUES ($1, $2, 'owner')",
    )
    .bind(slug)
    .bind(summary)
    .execute(pool)
    .await?;
    Ok(summary)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn citations_carry_the_importing_author(pool: PgPool) -> TestResult {
    crate::common::install_graphile(&pool).await?;
    for slug in ["acme", "mine"] {
        sqlx::query("INSERT INTO rh_wiki_topics (slug, title) VALUES ($1, $1)")
            .bind(slug)
            .execute(&pool)
            .await?;
    }
    let native = cite(&pool, "claude-code:native", "{}", "acme").await?;
    let imported = cite(
        &pool,
        IMPORT_KEY,
        r#"{"origin":{"author":"phil"},"receipt":{"verification":"verified"}}"#,
        "acme",
    )
    .await?;
    cite(&pool, "claude-code:other", "{}", "mine").await?;
    let memory = PostgresMemory::new(pool);

    let WikiLookup::Page(page) = memory
        .wiki_page(&WikiSlug::try_from("acme".to_owned())?)
        .await?
    else {
        return Err("the page".into());
    };
    let marked: Vec<(String, Option<&str>)> = page
        .citations
        .iter()
        .map(|citation| {
            (
                citation.summary_id.clone(),
                citation
                    .imported
                    .as_ref()
                    .and_then(|provenance| provenance.author.as_deref()),
            )
        })
        .collect();
    assert_eq!(marked.len(), 2);
    assert!(marked.contains(&(native.to_string(), None)));
    assert!(marked.contains(&(imported.to_string(), Some("phil"))));

    let authors = memory
        .wiki_imported_authors(vec!["acme".to_owned(), "mine".to_owned()])
        .await?;
    assert_eq!(authors.len(), 1);
    let acme = authors.get("acme").ok_or("authors of acme")?;
    assert_eq!(acme.len(), 1);
    assert_eq!(
        acme.first().map(|provenance| provenance.verification),
        Some(Verification::Verified)
    );
    Ok(())
}
