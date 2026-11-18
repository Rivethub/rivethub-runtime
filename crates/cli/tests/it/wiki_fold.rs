#![cfg(test)]
use wire::time::TimestampFormat as _;

use crate::wiki_support;

use std::path::Path;

use kernel::memory::MemoryStore as _;
use kernel::wiki::WikiLookup;
use kernel::wiki::consolidate::{
    ConsolidateWiki, WikiConsolidateStore as _, WikiFold, consolidation_plan,
};
use kernel::wiki::page::{CitationKind, FoldMark, PageCitation, PagePatch, WikiAction};
use kernel::wiki::page::{WikiHistoryEntry, WikiSlug};
use memory_postgres::PostgresMemory;
use memory_postgres::wiki_writer::WikiRepository;
use sqlx::PgPool;
use wiki_support::{TestResult, now, seed};

struct Seed<'a> {
    slug: &'a str,
    title: &'a str,
    state: &'a str,
    history: &'a str,
}

async fn seed_topic(
    pool: &PgPool,
    root: &Path,
    topic: &Seed<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let job = seed(pool).await?;
    let summary_id = job.summary_id.to_string();
    let (applied, released) = {
        let mut repository = WikiRepository::new(root.to_path_buf()).lock().await?;
        let applied = repository
            .apply(
                PagePatch {
                    action: WikiAction::Create,
                    slug: topic.slug.to_owned(),
                    verified_at: now(),
                    title: Some(topic.title.to_owned()),
                    add_aliases: Vec::new(),
                    add_tags: vec!["mesh".to_owned()],
                    add_entities: Vec::new(),
                    add_related: Vec::new(),
                    summary_delta: Some(topic.state.to_owned()),
                    current_state: None,
                    article: None,
                    article_patches: Vec::new(),
                    history_entry: Some(WikiHistoryEntry {
                        date: now().utc_date(),
                        title: topic.history.to_owned(),
                        body: format!("{} happened.", topic.history),
                    }),
                    add_citations: vec![PageCitation {
                        summary_id: summary_id.clone(),
                        date: Some(now().utc_date()),
                        kind: Some(CitationKind::Leaf),
                        note: None,
                    }],
                    add_sources: Vec::new(),
                    allow_shrink: false,
                },
                &summary_id,
                None,
            )
            .await;
        (applied, repository.release().await)
    };
    released?;
    let applied = applied?;
    sqlx::query(
        "INSERT INTO rh_wiki_topics (slug, title, current_state, search_text, history_count, git_sha)
         VALUES ($1, $2, $3, $3, 1, $4)",
    )
    .bind(topic.slug)
    .bind(topic.title)
    .bind(topic.state)
    .bind(&applied.git_sha)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO rh_wiki_citations (topic_slug, summary_id, kind, owner_user_id) VALUES ($1, $2, 'summary', 'owner')",
    )
    .bind(topic.slug)
    .bind(job.summary_id)
    .execute(pool)
    .await?;
    Ok(())
}

fn defaults() -> Result<ConsolidateWiki, serde_json::Error> {
    serde_json::from_str("{}")
}

async fn cell(pool: &PgPool, sql: &'static str) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(sql).fetch_one(pool).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn prefixed_duplicates_fold_into_the_plain_page_and_redirect(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = wiki.path().join("wiki");
    for topic in [
        Seed {
            slug: "mesh",
            title: "Mesh",
            state: "The mesh links every node.",
            history: "Mesh formed",
        },
        Seed {
            slug: "rivetos-mesh",
            title: "RivetOS mesh",
            state: "The mesh renews its certificates every seven days through step-ca.",
            history: "Certificates moved",
        },
    ] {
        seed_topic(&pool, &root, &topic).await?;
    }
    sqlx::query(
        "INSERT INTO rh_wiki_redirects (from_slug, to_slug) VALUES ('old-mesh', 'rivetos-mesh')",
    )
    .execute(&pool)
    .await?;
    let store = PostgresMemory::new(pool.clone()).with_wiki_directory(root.clone());
    let plan = consolidation_plan(&store.wiki_topic_stats().await?, &defaults()?);
    assert_eq!(
        plan,
        vec![WikiFold {
            cluster: "mesh".to_owned(),
            parent: "mesh".to_owned(),
            children: vec!["rivetos-mesh".to_owned()],
            mark: FoldMark::Merged,
        }]
    );
    let fold = plan.first().ok_or("one fold")?;
    assert_eq!(store.fold_wiki(fold, now()).await?, vec!["rivetos-mesh"]);

    let slugs: Vec<String> = sqlx::query_scalar("SELECT slug FROM rh_wiki_topics")
        .fetch_all(&pool)
        .await?;
    assert_eq!(slugs, vec!["mesh"]);
    assert_eq!(
        cell(
            &pool,
            "SELECT count(*)::text FROM rh_wiki_citations WHERE topic_slug = 'mesh'"
        )
        .await?,
        "2"
    );
    assert_eq!(
        cell(&pool, "SELECT string_agg(from_slug || '>' || to_slug, ',' ORDER BY from_slug) FROM rh_wiki_redirects").await?,
        "old-mesh>mesh,rivetos-mesh>mesh"
    );
    assert_eq!(
        cell(
            &pool,
            "SELECT array_to_string(aliases, ',') FROM rh_wiki_topics WHERE slug = 'mesh'"
        )
        .await?,
        "rivetos-mesh"
    );
    assert!(!root.join("topics/rivetos-mesh.md").exists());
    let markdown = tokio::fs::read_to_string(root.join("topics/mesh.md")).await?;
    assert!(markdown.contains("every seven days"));
    assert!(markdown.contains("#### Merged from rivetos-mesh"));
    assert!(markdown.contains("Certificates moved"));
    assert!(markdown.contains("Mesh formed"));
    assert!(matches!(
        store.wiki_page(&WikiSlug::try_from("rivetos-mesh".to_owned())?).await?,
        WikiLookup::Redirect(target) if target == "mesh"
    ));

    assert_eq!(store.fold_wiki(fold, now()).await?, Vec::<String>::new());
    assert_eq!(
        consolidation_plan(&store.wiki_topic_stats().await?, &defaults()?),
        Vec::<WikiFold>::new()
    );
    Ok(())
}
