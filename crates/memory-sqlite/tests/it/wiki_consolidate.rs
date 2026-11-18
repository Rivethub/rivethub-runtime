use jiff::Timestamp;
use std::path::Path;
use std::path::PathBuf;

use kernel::memory::MemoryStore;
use kernel::wiki::WikiFilter;
use kernel::wiki::WikiLookup;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::consolidate::WikiConsolidateStore;
use kernel::wiki::consolidate::WikiFold;
use kernel::wiki::page::FoldMark;
use kernel::wiki::page::WikiSlug;
use memory_sqlite::SqliteStore;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const PARENT: &str = "acmekit-deploys";
const CHILD: &str = "acmekit-deploys-friday-rollout";
const SUMMARY: &str = "00000000-0000-4000-8000-000000000001";
const STAMP: &str = "2026-10-01T00:00:00.000Z";
const CHILD_STATE: &str = "The Friday rollout ran behind the feature flag and finished by noon.";

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    store: SqliteStore,
}

struct Seed<'a> {
    slug: &'a str,
    title: &'a str,
    state: &'a str,
    article: &'a str,
    aliases: &'a str,
    tags: &'a str,
    related: &'a str,
    history: i64,
}

fn fixture() -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    seed(&path)?;
    let store = SqliteStore::open(&path)?;
    Ok(Fixture {
        _dir: dir,
        path,
        store,
    })
}

fn seed(path: &Path) -> TestResult {
    let conn = rusqlite::Connection::open(path)?;
    for topic in [
        Seed {
            slug: PARENT,
            title: "AcmeKit deploys",
            state: "AcmeKit deploys on Fridays.",
            article: "Deploy notes.",
            aliases: "[]",
            tags: r#"["ops"]"#,
            related: "[]",
            history: 1,
        },
        Seed {
            slug: CHILD,
            title: "Friday rollout",
            state: CHILD_STATE,
            article: "",
            aliases: r#"["friday-rollout"]"#,
            tags: r#"["ops","release"]"#,
            related: r#"["acmekit-deploys","staging-database"]"#,
            history: 2,
        },
    ] {
        conn.execute(
            "INSERT INTO ros_wiki_topics (slug, title, aliases, tags, related, current_state,
                article, search_text, history_count, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?6, ?8, ?9, ?9)",
            rusqlite::params![
                topic.slug,
                topic.title,
                topic.aliases,
                topic.tags,
                topic.related,
                topic.state,
                topic.article,
                topic.history,
                STAMP
            ],
        )?;
    }
    conn.execute(
        "INSERT INTO ros_wiki_redirects (from_slug, to_slug, created_at) VALUES (?1, ?2, ?3)",
        ("friday-rollout", CHILD, STAMP),
    )?;
    conn.execute(
        "INSERT INTO ros_wiki_citations (topic_slug, summary_id, kind, note, cited_at)
         VALUES (?1, ?2, 'summary', 'rolled out', ?3)",
        (CHILD, SUMMARY, STAMP),
    )?;
    conn.execute(
        "INSERT INTO ros_wiki_provenance (topic_slug, source_kind, source_id, created_at)
         VALUES (?1, 'summary', ?2, ?3)",
        (CHILD, SUMMARY, STAMP),
    )?;
    Ok(())
}

fn fold() -> WikiFold {
    WikiFold {
        cluster: PARENT.to_owned(),
        parent: PARENT.to_owned(),
        children: vec![CHILD.to_owned()],
        mark: FoldMark::Merged,
    }
}

async fn slugs(store: &SqliteStore) -> TestResult<Vec<String>> {
    Ok(store
        .wiki_topic_stats()
        .await?
        .into_iter()
        .map(|topic| topic.slug)
        .collect())
}

fn cell(path: &Path, sql: &str) -> TestResult<String> {
    let conn = rusqlite::Connection::open(path)?;
    Ok(conn.query_row(sql, [], |row| row.get(0))?)
}

async fn lookup(store: &SqliteStore, slug: &str) -> TestResult<WikiLookup> {
    Ok(store
        .wiki_page(&WikiSlug::try_from(slug.to_owned())?)
        .await?)
}

#[tokio::test]
async fn folding_moves_the_child_into_the_parent() -> TestResult {
    let fixture = fixture()?;
    let folded = fixture.store.fold_wiki(&fold(), Timestamp::now()).await?;
    assert_eq!(folded, vec![CHILD.to_owned()]);
    assert_eq!(slugs(&fixture.store).await?, vec![PARENT.to_owned()]);
    let row = |column: &str| {
        cell(
            &fixture.path,
            &format!("SELECT {column} FROM ros_wiki_topics WHERE slug = 'acmekit-deploys'"),
        )
    };
    assert_eq!(
        row("aliases")?,
        r#"["acmekit-deploys-friday-rollout","friday-rollout"]"#
    );
    assert_eq!(row("tags")?, r#"["ops","release"]"#);
    assert_eq!(row("related")?, r#"["staging-database"]"#);
    assert_eq!(row("current_state")?, CHILD_STATE);
    assert_eq!(
        row("article")?,
        "Deploy notes.\n\n#### Superseded parent text\n\n**Displaced parent summary:**\n\n\
         AcmeKit deploys on Fridays.\n\n#### Merged from acmekit-deploys-friday-rollout\n\n\
         **Friday rollout**"
    );
    assert_eq!(row("CAST(history_count AS TEXT)")?, "3");
    assert_eq!(
        cell(
            &fixture.path,
            "SELECT note FROM ros_wiki_citations WHERE topic_slug = 'acmekit-deploys'"
        )?,
        "rolled out"
    );
    assert_eq!(
        cell(
            &fixture.path,
            "SELECT source_id FROM ros_wiki_provenance WHERE topic_slug = 'acmekit-deploys'"
        )?,
        SUMMARY
    );
    Ok(())
}

#[tokio::test]
async fn the_folded_slug_and_its_aliases_redirect_to_the_parent() -> TestResult {
    let fixture = fixture()?;
    fixture.store.fold_wiki(&fold(), Timestamp::now()).await?;
    for slug in [CHILD, "friday-rollout"] {
        match lookup(&fixture.store, slug).await? {
            WikiLookup::Redirect(target) => assert_eq!(target, PARENT),
            WikiLookup::Page(_) | WikiLookup::Missing => {
                return Err(format!("{slug} does not redirect").into());
            }
        }
    }
    let found = fixture
        .store
        .wiki_topics(WikiFilter {
            query: Some("noon".to_owned()),
            tag: None,
            limit: 10,
            offset: 0,
        })
        .await?;
    let slugs: Vec<&str> = found
        .topics
        .iter()
        .map(|topic| topic.slug.as_str())
        .collect();
    assert_eq!(slugs, vec![PARENT]);
    Ok(())
}

#[tokio::test]
async fn a_repeated_fold_changes_nothing() -> TestResult {
    let fixture = fixture()?;
    fixture.store.fold_wiki(&fold(), Timestamp::now()).await?;
    let article = cell(
        &fixture.path,
        "SELECT article FROM ros_wiki_topics WHERE slug = 'acmekit-deploys'",
    )?;
    let again = fixture.store.fold_wiki(&fold(), Timestamp::now()).await?;
    assert_eq!(again, Vec::<String>::new());
    assert_eq!(
        cell(
            &fixture.path,
            "SELECT article FROM ros_wiki_topics WHERE slug = 'acmekit-deploys'"
        )?,
        article
    );
    Ok(())
}

#[tokio::test]
async fn a_missing_parent_is_an_error() -> TestResult {
    let fixture = fixture()?;
    let absent = WikiFold {
        cluster: "staging-database".to_owned(),
        parent: "staging-database".to_owned(),
        children: vec![CHILD.to_owned()],
        mark: FoldMark::Merged,
    };
    match fixture.store.fold_wiki(&absent, Timestamp::now()).await {
        Err(WikiStoreError::TopicMissing(slug)) => assert_eq!(slug, "staging-database"),
        other => return Err(format!("unexpected {other:?}").into()),
    }
    assert_eq!(
        slugs(&fixture.store).await?,
        vec![PARENT.to_owned(), CHILD.to_owned()]
    );
    Ok(())
}
