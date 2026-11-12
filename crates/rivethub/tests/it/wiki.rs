use std::sync::Arc;
use std::time::Duration;

use gateway::Gateway;
use kernel::memory::MemoryStore;
use kernel::wiki::WikiFilter;
use kernel::wiki::WikiLookup;
use kernel::wiki::page::WikiSlug;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use wire::gateway::wiki::WikiGapsResponse;
use wire::gateway::wiki::WikiPageResponse;
use wire::gateway::wiki::WikiSourceKind;
use wire::gateway::wiki::WikiTopicList;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn slugs(topics: &[kernel::wiki::WikiTopic]) -> Vec<&str> {
    topics.iter().map(|topic| topic.slug.as_str()).collect()
}

async fn seed(pool: &PgPool) -> TestResult {
    sqlx::query(
        "INSERT INTO rh_wiki_topics (slug, title, aliases, tags, entities, current_state, article,
         search_text, related, git_sha, last_verified_at, updated_at) VALUES
         ('release', 'Release guide', ARRAY['Rollout'], ARRAY['project:AcmeKit'],
          ARRAY['missing', 'missing', 'Known', 'legacy', 'Rollout'], $1, 'Article with <script> and quotes',
          'Release rollout checklist', ARRAY['known'], 'abc123',
          '2026-10-01T00:00:00Z', '2026-10-14T00:00:00Z'),
         ('known', 'Known', ARRAY[]::text[], ARRAY[]::text[], ARRAY['missing'], 'Known topic', '',
          'Known topic', ARRAY[]::text[], NULL, NULL, '2026-10-12T00:00:00Z')",
    )
    .bind("🚀".repeat(241))
    .execute(pool)
    .await?;
    sqlx::query("INSERT INTO rh_wiki_redirects (from_slug, to_slug) VALUES ('legacy', 'release')")
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO rh_wiki_citations (topic_slug, summary_id, kind, note, cited_at, owner_user_id)
         VALUES ('release', '00000000-0000-4000-8000-000000000001', 'decision', 'Ship it', '2026-10-01T00:00:00Z', 'owner')",
    ).execute(pool).await?;
    sqlx::query(
        "INSERT INTO rh_wiki_provenance (topic_slug, source_kind, source_id, conversation_id, owner_user_id)
         VALUES ('release', 'summary', '00000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000002', 'owner')",
    ).execute(pool).await?;
    Ok(())
}

async fn get(
    client: &reqwest::Client,
    base: &str,
    target: &str,
) -> Result<reqwest::Response, reqwest::Error> {
    client.get(format!("{base}{target}")).send().await
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario; the sqlx::test macro had hidden its length"
)]
async fn wiki_routes_read_pages_and_metadata_from_postgres() -> TestResult {
    let db = crate::pgdb::TestDb::migrated().await?;
    let pool = db.pool();
    seed(&pool).await?;
    let home = tempfile::tempdir()?;
    let memory = Arc::new(PostgresMemory::new(pool));
    let gateway = Arc::new(
        Gateway::new(
            memory,
            gateway::Settings {
                hostname: "wiki-test".to_owned(),
                node: "wiki-test".to_owned(),
                allowed_origins: Vec::new(),
                allowed_hosts: Vec::new(),
                static_dir: None,
                root_redirect: None,
                state_dir: home.path().join("state"),
                private_ca: None,
                request_deadline: gateway::REQUEST_DEADLINE,
            },
        )
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
    let response = get(&client, &base, "/api/wiki/release").await?;
    assert_eq!(response.status().as_u16(), 200);
    let actual: WikiPageResponse = response.json().await?;
    assert_eq!(actual.slug, "release");
    assert_eq!(actual.current_state, "🚀".repeat(241));
    assert_eq!(
        actual.article.as_deref(),
        Some("Article with <script> and quotes")
    );
    assert_eq!(actual.related, Some(vec!["known".to_owned()]));
    assert_eq!(
        actual.updated_at,
        chrono::DateTime::parse_from_rfc3339("2026-10-14T00:00:00.000Z")
            .expect("instant")
            .with_timezone(&chrono::Utc)
    );
    assert_eq!(
        actual.last_verified,
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-10-01T00:00:00.000Z")
                .expect("instant")
                .with_timezone(&chrono::Utc)
        )
    );
    let citation = actual.citations.first().ok_or("one citation")?;
    let source = actual.sources.first().ok_or("one source")?;
    assert_eq!(citation.note.as_deref(), Some("Ship it"));
    assert_eq!(source.kind, WikiSourceKind::Summary);
    assert_eq!(source.ids, ["00000000-0000-4000-8000-000000000001"]);
    assert!(actual.markdown.starts_with("# Release guide\n"));
    let response = get(&client, &base, "/api/wiki/release/raw").await?;
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .ok_or("raw content type")?
            .to_str()?,
        "text/markdown; charset=utf-8"
    );
    assert_eq!(response.text().await?, actual.markdown);
    let response = get(&client, &base, "/api/wiki/_gaps?limit=5").await?;
    assert_eq!(response.status().as_u16(), 200);
    let gaps: WikiGapsResponse = response.json().await?;
    assert_eq!(
        gaps.stalest
            .iter()
            .map(|topic| topic.slug.as_str())
            .collect::<Vec<_>>(),
        ["release", "known"]
    );
    for retired in ["/api/wiki/page/release", "/api/wiki/gaps"] {
        assert_eq!(get(&client, &base, retired).await?.status().as_u16(), 404);
    }
    let response = get(&client, &base, "/api/wiki/topics?tag=project%3AAcmeKit").await?;
    let topics: WikiTopicList = response.json().await?;
    assert_eq!(topics.total, 1);
    assert_eq!(
        topics.topics.first().ok_or("one topic")?.excerpt,
        "🚀".repeat(240)
    );
    let response = get(&client, &base, "/wiki/release").await?;
    assert_eq!(response.status().as_u16(), 200);
    let html = response.text().await?;
    assert!(html.contains("Article with &lt;script&gt; and quotes"));
    assert!(html.contains("Ship it"));
    for (target, location) in [
        ("/api/wiki/legacy", "/api/wiki/release"),
        ("/api/wiki/legacy/raw", "/api/wiki/release/raw"),
    ] {
        let response = get(&client, &base, target).await?;
        assert_eq!(response.status().as_u16(), 308);
        assert_eq!(
            response
                .headers()
                .get("location")
                .ok_or("redirect location")?
                .to_str()?,
            location
        );
    }
    drop(shutdown_guard);
    tokio::time::timeout(Duration::from_secs(30), servers.join_next())
        .await?
        .ok_or("server task")???;
    Ok(())
}

#[tokio::test]
async fn filters_bind_input_and_empty_pages_keep_total() -> TestResult {
    let db = crate::pgdb::TestDb::migrated().await?;
    let pool = db.pool();
    seed(&pool).await?;
    let store = PostgresMemory::new(pool);
    let mut filter = WikiFilter {
        query: None,
        tag: None,
        limit: 1,
        offset: 1,
    };
    let topics = store.wiki_topics(filter.clone()).await?;
    assert_eq!(topics.total, 2);
    assert_eq!(topics.topics.first().ok_or("one topic")?.slug, "known");
    filter.offset = 999;
    let topics = store.wiki_topics(filter.clone()).await?;
    assert_eq!(topics.total, 2);
    assert_eq!(topics.topics.len(), 0);
    filter.offset = 0;
    filter.query = Some("checklist".to_owned());
    assert_eq!(
        store
            .wiki_topics(filter.clone())
            .await?
            .topics
            .first()
            .ok_or("one topic")?
            .slug,
        "release"
    );
    filter.query = Some("' OR true --".to_owned());
    assert_eq!(store.wiki_topics(filter.clone()).await?.total, 0);
    filter.query = None;
    filter.tag = Some("' OR true --".to_owned());
    assert_eq!(store.wiki_topics(filter).await?.total, 0);
    let slug = WikiSlug::try_from("' OR true --".to_owned())?;
    assert_eq!(store.wiki_page(&slug).await?, WikiLookup::Missing);
    Ok(())
}

#[tokio::test]
async fn gaps_resolve_titles_aliases_and_redirects_and_deduplicate_references() -> TestResult {
    let db = crate::pgdb::TestDb::migrated().await?;
    let pool = db.pool();
    seed(&pool).await?;
    let store = PostgresMemory::new(pool);
    let gaps = store.wiki_gaps(50).await?;
    assert_eq!(gaps.red_links.len(), 1);
    let missing = gaps.red_links.first().ok_or("one red link")?;
    assert_eq!(missing.entity, "missing");
    assert_eq!(missing.referenced_by, ["known", "release"]);
    assert_eq!(slugs(&gaps.stalest), ["release", "known"]);
    assert_eq!(slugs(&store.wiki_gaps(1).await?.stalest), ["release"]);
    Ok(())
}

#[tokio::test]
async fn absent_wiki_tables_are_errors_not_empty_results() {
    let db = crate::pgdb::TestDb::bare().await.expect("test database");
    let pool = db.pool();
    let store = PostgresMemory::new(pool);
    let slug = WikiSlug::try_from("release".to_owned()).unwrap();
    assert!(store.wiki_page(&slug).await.is_err());
    assert!(store.wiki_gaps(10).await.is_err());
    assert!(
        store
            .wiki_topics(WikiFilter {
                query: None,
                tag: None,
                limit: 50,
                offset: 0
            })
            .await
            .is_err()
    );
}
