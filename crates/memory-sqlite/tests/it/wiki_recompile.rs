use memory_sqlite::SqliteStore;
use testkit::wiki_files::CONVERSATION;

use crate::common::TestResult;

#[tokio::test]
async fn recompile_restores_pushed_out_history_and_archives_copies() -> TestResult {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("memory.sqlite");
    migrate::sqlite(&database)?;
    rusqlite::Connection::open(&database)?.execute(
        "INSERT INTO ros_conversations
            (id, session_key, agent, title, created_at, updated_at, owner_user_id)
         VALUES (?1, 'claude-code:recompile', 'rivet', 'Recompile',
                 '2026-10-29T12:00:00Z', '2026-10-29T12:00:00Z', 'owner')",
        [CONVERSATION],
    )?;
    let root = std::fs::canonicalize(directory.path())?;
    let store = SqliteStore::open(&database)?.with_wiki_directory(root.clone());
    testkit::wiki_recompile::restores_history_and_archives_copies(&store, &root).await?;
    testkit::wiki_recompile::validates_cited_summaries(&store, &root).await
}

fn git(root: &std::path::Path, args: &[&str]) -> TestResult<String> {
    let out = std::process::Command::new("git")
        .current_dir(root)
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

fn committed_page(
    root: &std::path::Path,
    slug: &kernel::wiki::page::WikiSlug,
    state: &str,
    article: &str,
) -> TestResult<kernel::wiki::page::WikiPage> {
    std::fs::create_dir_all(root.join("topics"))?;
    git(root, &["init", "-q", "-b", "main"])?;
    let mut page = kernel::wiki::page::WikiPage::empty(slug.clone(), "Page".to_owned());
    state.clone_into(&mut page.current_state);
    article.clone_into(&mut page.article);
    std::fs::write(
        root.join(format!("topics/{}.md", slug.as_ref())),
        kernel::wiki::page::serialize_wiki_page(&page)?,
    )?;
    git(root, &["add", "--", "topics"])?;
    git(root, &["commit", "-q", "-m", "first"])?;
    Ok(page)
}

#[tokio::test]
async fn recompile_starts_from_a_page_that_is_only_in_the_database() -> TestResult {
    use kernel::wiki::recompile::WikiRecompileStore as _;
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("memory.sqlite");
    migrate::sqlite(&database)?;
    let root = std::fs::canonicalize(directory.path())?.join("wiki");
    let slug = kernel::wiki::page::WikiSlug::try_from("legacy".to_owned())?;
    committed_page(&root, &slug, "File summary.", "File article.")?;
    rusqlite::Connection::open(&database)?.execute(
        "INSERT INTO ros_wiki_topics (slug, title, current_state, article, search_text, created_at, updated_at)
         VALUES ('legacy', 'Page', 'Database summary.', 'Database article.', 'legacy',
                 '2026-10-10T12:00:00.000Z', '2026-10-10T12:00:00.000Z')",
        [],
    )?;
    let store = SqliteStore::open(&database)?.with_wiki_directory(root.clone());
    let source = store
        .recompile_source(&slug, "2026-01-01T00:00:00Z".parse()?)
        .await?
        .ok_or("no source")?;
    assert_eq!(
        (
            source.page.current_state.as_str(),
            source.page.article.as_str()
        ),
        ("Database summary.", "Database article.")
    );
    let sha = store
        .store_recompiled(
            kernel::wiki::recompile::Recompiled {
                inputs: Vec::new(),
                base: source.page.clone(),
                page: source.page,
                archived: Vec::new(),
                message: "recompile".to_owned(),
            },
            "2026-10-30T12:00:00Z".parse()?,
        )
        .await?;
    let file = kernel::wiki::page::parse_wiki_page(&std::fs::read_to_string(
        root.join("topics/legacy.md"),
    )?)?;
    assert_eq!(
        (file.current_state.as_str(), file.article.as_str()),
        ("Database summary.", "Database article.")
    );
    assert_eq!(sha, git(&root, &["rev-parse", "HEAD"])?.trim());
    assert_eq!(
        git(&root, &["log", "--format=%s"])?
            .lines()
            .filter(|subject| subject.contains("record the stored page"))
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn recompile_keeps_a_change_another_writer_committed_meanwhile() -> TestResult {
    use kernel::wiki::recompile::WikiRecompileStore as _;
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("memory.sqlite");
    migrate::sqlite(&database)?;
    let root = std::fs::canonicalize(directory.path())?.join("wiki");
    let slug = kernel::wiki::page::WikiSlug::try_from("busy".to_owned())?;
    committed_page(&root, &slug, "First summary.", "First article.")?;
    let store = SqliteStore::open(&database)?.with_wiki_directory(root.clone());
    let source = store
        .recompile_source(&slug, "2026-01-01T00:00:00Z".parse()?)
        .await?
        .ok_or("no source")?;
    let mut other = source.page.clone();
    "First article.\n\nAdded by another writer.".clone_into(&mut other.article);
    std::fs::write(
        root.join("topics/busy.md"),
        kernel::wiki::page::serialize_wiki_page(&other)?,
    )?;
    git(&root, &["commit", "-q", "-am", "other writer"])?;
    let mut ours = source.page.clone();
    "Recompiled summary.".clone_into(&mut ours.current_state);
    let sha = store
        .store_recompiled(
            kernel::wiki::recompile::Recompiled {
                inputs: Vec::new(),
                base: source.page,
                page: ours,
                archived: Vec::new(),
                message: "recompile".to_owned(),
            },
            "2026-10-30T12:00:00Z".parse()?,
        )
        .await?;
    let file = kernel::wiki::page::parse_wiki_page(&std::fs::read_to_string(
        root.join("topics/busy.md"),
    )?)?;
    assert_eq!(sha, git(&root, &["rev-parse", "HEAD"])?.trim());
    assert_eq!(file.current_state, "Recompiled summary.");
    assert!(
        file.article.contains("Added by another writer."),
        "{}",
        file.article
    );
    Ok(())
}
