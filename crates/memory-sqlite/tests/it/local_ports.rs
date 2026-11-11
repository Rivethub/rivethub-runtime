use std::path::Path;
use std::path::PathBuf;

use chrono::Utc;
use kernel::wiki::builder::ExtractWiki;
use kernel::wiki::builder::WikiBuilderStore;
use kernel::wiki::builder::WikiRouting;
use kernel::wiki::page::WikiAction;
use kernel::wiki::page::WikiPatch;
use kernel::wiki::page::WikiSlug;
use memory_sqlite::SqliteStore;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    store: SqliteStore,
}

fn fixture() -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let store = SqliteStore::open(&path)?;
    Ok(Fixture {
        _dir: dir,
        path,
        store,
    })
}

fn summary_id() -> TestResult<Uuid> {
    Ok(Uuid::parse_str("0199e2a0-7b3c-7a00-8000-000000000001")?)
}

fn seed_summary(path: &Path, id: Uuid) -> TestResult {
    let conn = rusqlite::Connection::open(path)?;
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    conn.execute(
        "INSERT OR IGNORE INTO ros_conversations
             (id, session_key, agent, created_at, updated_at, owner_user_id)
         VALUES ('00000000-0000-4000-8000-0000000000c1', 'cli:local', 'rivet', ?1, ?1, 'owner')",
        [&now],
    )?;
    conn.execute(
        "INSERT INTO ros_summaries (id, conversation_id, content, created_at)
         VALUES (?1, '00000000-0000-4000-8000-0000000000c1', ?2, ?3)",
        (id.to_string(), "AcmeKit deploys on Fridays.", now),
    )?;
    Ok(())
}

fn patch() -> TestResult<WikiPatch> {
    Ok(WikiPatch {
        action: WikiAction::Create,
        slug: WikiSlug::try_from("acmekit-deploys".to_owned())?,
        title: "AcmeKit deploys".to_owned(),
        aliases: Vec::new(),
        tags: Vec::new(),
        entities: Vec::new(),
        summary: "AcmeKit deploys on Fridays.".to_owned(),
        article: None,
        related: None,
        history_entry: None,
        entity_type: None,
        facts: None,
    })
}

fn job(id: Uuid) -> ExtractWiki {
    ExtractWiki {
        summary_id: id,
        conversation_id: None,
    }
}

fn cell(path: &Path, sql: &str, id: &str) -> TestResult<String> {
    let conn = rusqlite::Connection::open(path)?;
    let value = conn.query_row(sql, [id], |row| row.get(0))?;
    Ok(value)
}

#[tokio::test]
async fn apply_wiki_stores_the_topic_and_a_repeat_returns_it() -> TestResult {
    let fixture = fixture()?;
    let id = summary_id()?;
    seed_summary(&fixture.path, id)?;
    let created = fixture
        .store
        .apply_wiki(
            &job(id),
            vec![patch()?],
            &WikiRouting::default(),
            Utc::now(),
        )
        .await?;
    assert_eq!(created, vec!["acmekit-deploys".to_owned()]);
    let title = cell(
        &fixture.path,
        "SELECT title FROM ros_wiki_topics WHERE slug = ?1",
        "acmekit-deploys",
    )?;
    assert_eq!(title, "AcmeKit deploys");
    let again = fixture
        .store
        .apply_wiki(
            &job(id),
            vec![patch()?],
            &WikiRouting::default(),
            Utc::now(),
        )
        .await?;
    assert_eq!(again, created);
    Ok(())
}

#[tokio::test]
async fn apply_wiki_adds_a_resolved_patch_as_a_note() -> TestResult {
    let fixture = fixture()?;
    let first = summary_id()?;
    let second = Uuid::parse_str("0199e2a0-7b3c-7a00-8000-000000000002")?;
    seed_summary(&fixture.path, first)?;
    seed_summary(&fixture.path, second)?;
    fixture
        .store
        .apply_wiki(
            &job(first),
            vec![patch()?],
            &WikiRouting::default(),
            Utc::now(),
        )
        .await?;
    let other = WikiPatch {
        slug: WikiSlug::try_from("label-printer-jam".to_owned())?,
        title: "Label printer jam".to_owned(),
        summary: "The label printer jams on Mondays.".to_owned(),
        aliases: vec![WikiSlug::try_from("acmekit-deploys".to_owned())?],
        ..patch()?
    };
    let touched = fixture
        .store
        .apply_wiki(
            &job(second),
            vec![other],
            &WikiRouting::default(),
            Utc::now(),
        )
        .await?;
    assert_eq!(touched, vec!["acmekit-deploys".to_owned()]);
    let title = cell(
        &fixture.path,
        "SELECT title FROM ros_wiki_topics WHERE slug = ?1",
        "acmekit-deploys",
    )?;
    assert_eq!(title, "AcmeKit deploys");
    let state = cell(
        &fixture.path,
        "SELECT current_state FROM ros_wiki_topics WHERE slug = ?1",
        "acmekit-deploys",
    )?;
    assert_eq!(state, "AcmeKit deploys on Fridays.");
    Ok(())
}

#[tokio::test]
async fn fail_wiki_does_not_clobber_a_done_extraction() -> TestResult {
    let fixture = fixture()?;
    let id = summary_id()?;
    seed_summary(&fixture.path, id)?;
    fixture
        .store
        .apply_wiki(
            &job(id),
            vec![patch()?],
            &WikiRouting::default(),
            Utc::now(),
        )
        .await?;
    fixture
        .store
        .fail_wiki(id, "later failure", Utc::now())
        .await?;
    let status = cell(
        &fixture.path,
        "SELECT status FROM ros_wiki_extractions WHERE summary_id = ?1",
        &id.to_string(),
    )?;
    assert_eq!(status, "done");
    Ok(())
}
