use kernel::wiki::builder::ExtractWiki;
use kernel::wiki::builder::WikiBuilderStore;
use kernel::wiki::builder::WikiRouting;
use kernel::wiki::page::WikiAction;
use kernel::wiki::page::WikiPatch;
use kernel::wiki::page::WikiSlug;
use memory_sqlite::SqliteStore;
use testkit::wiki_files::CONVERSATION;

use crate::common::TestResult;

const SUMMARY: &str = "00000000-0000-4000-8000-0000000000c1";

fn patch(slug: &str, aliases: &[&str], summary: &str) -> TestResult<WikiPatch> {
    let mut named = Vec::new();
    for alias in aliases {
        named.push(WikiSlug::try_from((*alias).to_owned())?);
    }
    Ok(WikiPatch {
        action: WikiAction::Update,
        slug: WikiSlug::try_from(slug.to_owned())?,
        title: slug.to_owned(),
        aliases: named,
        tags: Vec::new(),
        entities: Vec::new(),
        summary: summary.to_owned(),
        article: None,
        related: None,
        history_entry: None,
        entity_type: None,
        facts: None,
    })
}

fn seeded(directory: &std::path::Path) -> TestResult<std::path::PathBuf> {
    let database = directory.join("memory.sqlite");
    migrate::sqlite(&database)?;
    let conn = rusqlite::Connection::open(&database)?;
    conn.execute(
        "INSERT INTO ros_conversations
            (id, session_key, agent, title, created_at, updated_at, owner_user_id)
         VALUES (?1, 'claude-code:aliases', 'rivet', 'aliases', '2026-10-10T12:00:00.000Z',
                 '2026-10-10T12:00:00.000Z', 'owner')",
        [CONVERSATION],
    )?;
    for (slug, aliases) in [
        ("runtime", "[]"),
        ("upstream", "[]"),
        ("platform", "[]"),
        ("product", r#"["old-name"]"#),
    ] {
        conn.execute(
            "INSERT INTO ros_wiki_topics
                (slug, title, aliases, current_state, article, search_text, created_at, updated_at)
             VALUES (?1, ?1, ?2, 'Before.', 'Article.', ?1, '2026-10-10T12:00:00.000Z',
                     '2026-10-10T12:00:00.000Z')",
            rusqlite::params![slug, aliases],
        )?;
    }
    conn.execute(
        "INSERT INTO ros_wiki_redirects (from_slug, to_slug, created_at)
         VALUES ('old-name', 'platform', '2026-10-10T12:00:00.000Z')",
        [],
    )?;
    Ok(database)
}

fn strings(database: &std::path::Path, sql: &str) -> TestResult<Vec<String>> {
    let conn = rusqlite::Connection::open(database)?;
    let mut statement = conn.prepare(sql)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(rows)
}

#[tokio::test]
async fn an_alias_another_page_holds_does_not_stop_the_write() -> TestResult {
    let directory = tempfile::tempdir()?;
    let database = seeded(directory.path())?;
    let wiki = directory.path().join("wiki");
    std::fs::create_dir(&wiki)?;
    let store = SqliteStore::open(&database)?.with_wiki_directory(wiki);
    writes_land(&store, &database).await
}

#[tokio::test]
async fn an_alias_another_page_holds_does_not_stop_a_write_without_page_files() -> TestResult {
    let directory = tempfile::tempdir()?;
    let database = seeded(directory.path())?;
    let store = SqliteStore::open(&database)?;
    writes_land(&store, &database).await
}

async fn writes_land(store: &SqliteStore, database: &std::path::Path) -> TestResult {
    testkit::wiki_files::write_leaf(store, SUMMARY, "Notes.", "2026-10-10T12:00:00Z").await?;
    let job = ExtractWiki {
        summary_id: SUMMARY.parse()?,
        conversation_id: Some(CONVERSATION.parse()?),
    };
    store
        .apply_wiki(
            &job,
            vec![
                patch("upstream", &["runtime"], "Upstream now.")?,
                patch("product", &["old-name"], "Product now.")?,
                patch("old-name", &[], "Platform now.")?,
            ],
            &WikiRouting::default(),
            "2026-10-10T13:00:00Z".parse()?,
        )
        .await?;
    assert_eq!(
        strings(
            database,
            "SELECT DISTINCT topic_slug FROM ros_wiki_citations ORDER BY topic_slug"
        )?,
        ["platform", "product", "upstream"]
    );
    assert_eq!(
        strings(
            database,
            "SELECT from_slug || '>' || to_slug FROM ros_wiki_redirects ORDER BY from_slug"
        )?,
        ["old-name>platform"]
    );
    assert_eq!(
        strings(
            database,
            "SELECT aliases FROM ros_wiki_topics WHERE slug = 'upstream'"
        )?,
        ["[]"]
    );
    Ok(())
}
