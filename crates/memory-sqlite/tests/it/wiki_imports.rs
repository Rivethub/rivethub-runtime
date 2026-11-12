use kernel::memory::MemoryStore as _;
use kernel::memory::Verification;
use kernel::wiki::WikiLookup;
use kernel::wiki::page::WikiSlug;
use memory_sqlite::SqliteStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const IMPORT_KEY: &str = "import:9f2c0000000000000000000000000000000000000000000000000000000000e1";
const STAMP: &str = "2026-10-01T00:00:00.000Z";

fn cite(
    conn: &rusqlite::Connection,
    key: &str,
    settings: &str,
    slug: &str,
    n: u8,
) -> rusqlite::Result<String> {
    let conversation = format!("00000000-0000-4000-8000-00000000010{n}");
    let summary = format!("00000000-0000-4000-8000-00000000020{n}");
    conn.execute(
        "INSERT INTO ros_conversations (id, session_key, agent, channel, settings, created_at, updated_at)
         VALUES (?1, ?2, 'opus', 'claude-code', ?3, ?4, ?4)",
        (&conversation, key, settings, STAMP),
    )?;
    conn.execute(
        "INSERT INTO ros_summaries (id, conversation_id, content, created_at)
         VALUES (?1, ?2, 'text', ?3)",
        (&summary, &conversation, STAMP),
    )?;
    conn.execute(
        "INSERT INTO ros_wiki_citations (topic_slug, summary_id, kind, note, cited_at)
         VALUES (?1, ?2, 'summary', 'n', ?3)",
        (slug, &summary, STAMP),
    )?;
    Ok(summary)
}

#[tokio::test]
async fn citations_carry_the_importing_author() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let conn = rusqlite::Connection::open(&path)?;
    for slug in ["acme", "mine"] {
        conn.execute(
            "INSERT INTO ros_wiki_topics (slug, title, created_at, updated_at) VALUES (?1, ?1, ?2, ?2)",
            (slug, STAMP),
        )?;
    }
    let native = cite(&conn, "claude-code:native", "{}", "acme", 1)?;
    let imported = cite(
        &conn,
        IMPORT_KEY,
        r#"{"origin":{"author":"phil"},"receipt":{"verification":"verified"}}"#,
        "acme",
        2,
    )?;
    cite(&conn, "claude-code:other", "{}", "mine", 3)?;
    drop(conn);
    let store = SqliteStore::open(&path)?;

    let WikiLookup::Page(page) = store
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
    assert!(marked.contains(&(native, None)));
    assert!(marked.contains(&(imported, Some("phil"))));

    let authors = store
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
