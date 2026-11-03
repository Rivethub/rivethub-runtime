use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use kernel::memory::MemoryStore as _;
use kernel::wiki::builder::{WikiBuilderStore as _, WikiWriteError};
use kernel::wiki::merge::{MergeInput, Merged, PageMerger};
use kernel::wiki::page::bound::{ARCHIVED_TITLE, ARTICLE_BUDGET};
use kernel::wiki::page::{
    PagePatch, WikiAction, WikiHistoryEntry, WikiPage, WikiSlug, parse_wiki_page,
    serialize_wiki_page,
};
use kernel::wiki::recompile::{RecompileWiki, recompile};
use memory_postgres::PostgresMemory;
use memory_postgres::wiki_writer::{AppliedPage, WikiRepository};
use sqlx::PgPool;
use testkit::wiki_bounded::RecordingMerger;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SUMMARY_ID: &str = "00000000-0000-4000-8000-000000000001";

fn now() -> Result<DateTime<Utc>, chrono::ParseError> {
    "2026-10-30T12:00:00Z".parse()
}

fn patch(slug: &str, article: &str) -> Result<PagePatch, chrono::ParseError> {
    Ok(PagePatch {
        action: WikiAction::Create,
        slug: slug.to_owned(),
        verified_at: now()?,
        title: Some("AcmeKit".to_owned()),
        add_aliases: Vec::new(),
        add_tags: Vec::new(),
        add_entities: Vec::new(),
        add_related: Vec::new(),
        summary_delta: Some("AcmeKit ships on Fridays.".to_owned()),
        current_state: None,
        article: Some(article.to_owned()),
        article_patches: Vec::new(),
        history_entry: None,
        add_citations: Vec::new(),
        add_sources: Vec::new(),
        allow_shrink: false,
    })
}

async fn apply(root: &Path, patch: PagePatch) -> Result<AppliedPage, WikiWriteError> {
    let (result, released) = {
        let mut repository = WikiRepository::new(root.to_path_buf()).lock().await?;
        let result = repository.apply(patch, SUMMARY_ID, None).await;
        (result, repository.release().await)
    };
    released?;
    result
}

async fn git(root: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = tokio::process::Command::new("git")
        .current_dir(root)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .output()
        .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn entry(date: &str, title: &str, body: &str) -> Result<WikiHistoryEntry, chrono::ParseError> {
    Ok(WikiHistoryEntry {
        date: date.parse()?,
        title: title.to_owned(),
        body: body.to_owned(),
    })
}

#[tokio::test]
async fn a_rewrite_archives_what_it_dropped_behind_one_history_line() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("wiki");
    apply(
        &root,
        patch(
            "acmekit",
            "### Releases\n\nShips on Fridays.\n\n### Ports\n\nListens on 8080.",
        )?,
    )
    .await?;
    let inline = git(&root, &["rev-parse", "HEAD"]).await?;
    let applied = apply(
        &root,
        patch("acmekit", "### Releases\n\nShips on Fridays and Tuesdays.")?,
    )
    .await?;
    let page = applied.page;
    assert!(
        page.history
            .iter()
            .all(|kept| kept.title != "Superseded article")
    );
    let line = page.history.first().ok_or("an archive line")?;
    assert_eq!(line.title, ARCHIVED_TITLE);
    assert!(line.body.contains("/wiki/acmekit/archive/2026-10#a-"));
    assert!(line.body.contains(inline.get(..10).ok_or("sha")?));
    let archive = tokio::fs::read_to_string(root.join("archive/acmekit/2026-10.1.md")).await?;
    assert!(archive.contains("Listens on 8080."));
    assert!(archive.starts_with("# Archive: AcmeKit"));
    let files = git(&root, &["show", "--name-only", "--format=", "HEAD"]).await?;
    assert_eq!(files, "archive/acmekit/2026-10.1.md\ntopics/acmekit.md\n");
    let stored = tokio::fs::read_to_string(root.join("topics/acmekit.md")).await?;
    assert_eq!(parse_wiki_page(&stored)?, page);
    Ok(())
}

#[tokio::test]
async fn an_unmerged_update_keeps_one_lead() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("wiki");
    apply(&root, patch("acmekit", "Ships on Fridays.")?).await?;
    let restated = PagePatch {
        summary_delta: Some(
            "AcmeKit ships on Fridays from the build box. Currently, 2.0 is out.".to_owned(),
        ),
        ..patch("acmekit", "Ships on Fridays.")?
    };
    let page = apply(&root, restated).await?.page;
    assert_eq!(
        page.current_state,
        "AcmeKit ships on Fridays. Currently, 2.0 is out."
    );
    Ok(())
}

fn facts(count: usize) -> String {
    (0..count)
        .map(|index| {
            let words: Vec<String> = (0..24).map(|word| format!("f{index}w{word}")).collect();
            format!("Fact {index}: {}", words.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[tokio::test]
async fn archive_writes_go_to_small_parts_and_leave_the_month_file_alone() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("wiki");
    apply(&root, patch("acmekit", "AcmeKit ships on Fridays.")?).await?;
    let month = "# Archive: AcmeKit, 2026-10\n\n<a id=\"a-1\"></a>\n\n## old\n\nThe staging cluster listens on port 8080.\n";
    tokio::fs::create_dir_all(root.join("archive/acmekit")).await?;
    tokio::fs::write(root.join("archive/acmekit/2026-10.md"), month).await?;
    git(&root, &["add", "--", "archive"]).await?;
    git(
        &root,
        &["commit", "-q", "-m", "an archive from before parts"],
    )
    .await?;
    for (round, extra) in [(0usize, true), (1, false), (2, false)] {
        let mut article = facts(400)
            .replace("Fact ", &format!("Round {round} fact "))
            .replace(" f", &format!(" r{round}f"));
        if extra {
            article.push_str("\n\nThe staging cluster listens on port 8080.");
        }
        let mut update = patch("acmekit", &article)?;
        update.action = WikiAction::Update;
        apply(&root, update).await?;
    }
    assert_eq!(
        tokio::fs::read_to_string(root.join("archive/acmekit/2026-10.md")).await?,
        month
    );
    let first = tokio::fs::read_to_string(root.join("archive/acmekit/2026-10.1.md")).await?;
    let second = tokio::fs::read_to_string(root.join("archive/acmekit/2026-10.2.md")).await?;
    assert!(!first.starts_with("# Archive") && !second.starts_with("# Archive"));
    assert!(!first.contains("8080") && !second.contains("8080"));
    assert!(second.contains("r1f399w0"));
    let touched = git(&root, &["show", "--name-only", "--format=", "HEAD"]).await?;
    assert_eq!(touched, "archive/acmekit/2026-10.3.md\ntopics/acmekit.md\n");
    Ok(())
}

#[tokio::test]
async fn an_oversized_article_is_held_to_its_budget() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("wiki");
    let applied = apply(&root, patch("acmekit", &facts(200))?).await?;
    assert!(applied.page.article.len() <= ARTICLE_BUDGET);
    let archive = tokio::fs::read_to_string(root.join("archive/acmekit/2026-10.1.md")).await?;
    assert!(archive.contains("Fact 199:"));
    Ok(())
}

async fn commit_page(root: &Path, page: &WikiPage, message: &str) -> TestResult {
    tokio::fs::create_dir_all(root.join("topics")).await?;
    tokio::fs::write(
        root.join(format!("topics/{}.md", page.meta.slug.as_ref())),
        serialize_wiki_page(page)?,
    )
    .await?;
    git(root, &["add", "--", "topics"]).await?;
    git(root, &["commit", "-q", "-m", message]).await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn recompile_restores_pushed_out_history_and_archives_copies(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("wiki");
    tokio::fs::create_dir_all(&root).await?;
    git(&root, &["init", "-q", "-b", "main"]).await?;
    let slug = WikiSlug::try_from("acmekit".to_owned())?;
    let mut page = WikiPage::empty(slug.clone(), "AcmeKit".to_owned());
    page.current_state = "AcmeKit ships on Fridays.".to_owned();
    page.history = vec![
        entry("2026-10-29", "Moved the build box", "Builds moved.")?,
        entry("2026-10-28", "First release", "Shipped 1.0.")?,
    ];
    commit_page(&root, &page, "real history").await?;
    page.history = (0..4)
        .map(|index| entry("2026-10-30", "Superseded article", &format!("copy {index}")))
        .collect::<Result<Vec<_>, _>>()?;
    page.extra_sections = vec![kernel::wiki::page::ArticleSection {
        heading: "Desktop".to_owned(),
        body: (0..80)
            .map(|index| format!("### Note {index}\n\n{index} {}", "y".repeat(120)))
            .collect::<Vec<_>>()
            .join("\n\n"),
    }];
    commit_page(&root, &page, "copies pushed the history out").await?;
    let memory = PostgresMemory::new(pool.clone()).with_wiki_directory(root.clone());
    let merger = RecordingMerger::default();
    let job = RecompileWiki {
        slug: slug.clone(),
        since: "2026-10-01T00:00:00Z".parse()?,
    };
    let report = recompile(&memory, &merger, &job, now()?)
        .await?
        .ok_or("the page exists")?;
    assert_eq!(report.restored, 2);
    assert_eq!(report.merges, 2);
    let stored =
        parse_wiki_page(&tokio::fs::read_to_string(root.join("topics/acmekit.md")).await?)?;
    let titles: Vec<&str> = stored
        .history
        .iter()
        .map(|kept| kept.title.as_str())
        .collect();
    assert_eq!(
        titles,
        vec![ARCHIVED_TITLE, "Moved the build box", "First release"]
    );
    assert_eq!(stored.current_state, "AcmeKit ships on Fridays. Merged.");
    let archive = tokio::fs::read_to_string(root.join("archive/acmekit/2026-10.1.md")).await?;
    assert!(archive.contains("copy 3"));
    assert!(archive.contains("### Note 16"));
    assert!(archive.contains("### Note 79"));
    let sha: String =
        sqlx::query_scalar("SELECT git_sha FROM rh_wiki_topics WHERE slug = 'acmekit'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(sha, git(&root, &["rev-parse", "HEAD"]).await?.trim());
    Ok(())
}

fn rotated(round: usize, sentences: &[&str]) -> String {
    sentences
        .iter()
        .map(|sentence| {
            let mut words: Vec<&str> = sentence.split(' ').collect();
            let turn = round % words.len();
            words.rotate_left(turn);
            words.join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[tokio::test]
async fn fifty_rephrasings_archive_only_the_paragraph_that_left() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("wiki");
    let kept = [
        "AcmeKit releases ship every Friday from the build box after the health gate passes",
        "The deploy pipeline runs integration suites against staging before each production rollout",
    ];
    let dropped = "The staging cluster listens on port 8080 behind the internal proxy";
    for round in 0..50 {
        let mut sentences = kept.to_vec();
        if round < 25 {
            sentences.push(dropped);
        }
        let mut update = patch("acmekit", &rotated(round, &sentences))?;
        if round > 0 {
            update.action = WikiAction::Update;
        }
        apply(&root, update).await?;
    }
    let archive = tokio::fs::read_to_string(root.join("archive/acmekit/2026-10.1.md")).await?;
    assert_eq!(archive.matches("8080").count(), 1, "{archive}");
    assert!(!archive.contains("AcmeKit releases") && !archive.contains("integration suites"));
    let commits = git(&root, &["log", "--format=%h", "--", "archive"]).await?;
    assert_eq!(commits.lines().count(), 1);
    Ok(())
}

struct ConcurrentMerger {
    root: PathBuf,
    wrote: AtomicBool,
    inner: RecordingMerger,
}

#[async_trait::async_trait]
impl PageMerger for ConcurrentMerger {
    async fn merge(
        &self,
        input: &MergeInput,
    ) -> Result<Merged, Box<dyn std::error::Error + Send + Sync>> {
        if !self.wrote.swap(true, Ordering::SeqCst) {
            let mut update = patch(
                "acmekit",
                &format!("Port 8080 opened for staging.\n\n{}", facts(200)),
            )?;
            update.action = WikiAction::Update;
            update.summary_delta = None;
            apply(&self.root, update).await?;
        }
        self.inner.merge(input).await
    }
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_recompile_rebases_onto_a_write_that_landed_meanwhile(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("wiki");
    tokio::fs::create_dir_all(&root).await?;
    git(&root, &["init", "-q", "-b", "main"]).await?;
    let slug = WikiSlug::try_from("acmekit".to_owned())?;
    let mut page = WikiPage::empty(slug.clone(), "AcmeKit".to_owned());
    page.current_state = "AcmeKit ships on Fridays.".to_owned();
    page.article = facts(200);
    commit_page(&root, &page, "first").await?;
    let memory = PostgresMemory::new(pool.clone()).with_wiki_directory(root.clone());
    let merger = ConcurrentMerger {
        root: root.clone(),
        wrote: AtomicBool::new(false),
        inner: RecordingMerger::default(),
    };
    let job = RecompileWiki {
        slug,
        since: "2026-10-01T00:00:00Z".parse()?,
    };
    recompile(&memory, &merger, &job, now()?)
        .await?
        .ok_or("the page exists")?;
    let stored =
        parse_wiki_page(&tokio::fs::read_to_string(root.join("topics/acmekit.md")).await?)?;
    assert!(stored.article.starts_with("Port 8080 opened for staging."));
    assert!(
        stored
            .current_state
            .starts_with("AcmeKit ships on Fridays. Merged.")
    );
    let sha: String =
        sqlx::query_scalar("SELECT git_sha FROM rh_wiki_topics WHERE slug = 'acmekit'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(sha, git(&root, &["rev-parse", "HEAD"]).await?.trim());
    let slug = WikiSlug::try_from("acmekit".to_owned())?;
    let month = memory
        .wiki_archive(&slug, Some("2026-10"))
        .await?
        .ok_or("this month's archive")?;
    assert!(month.starts_with("# Archive: AcmeKit, 2026-10"));
    assert_eq!(memory.wiki_archive(&slug, None).await?, Some(month));
    assert_eq!(memory.wiki_archive(&slug, Some("2026-09")).await?, None);
    assert_eq!(memory.wiki_archive(&slug, Some("../x")).await?, None);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn one_writer_at_a_time_holds_a_page_route(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool);
    let lease = store
        .lease_route("rivethub#desktop")
        .await?
        .ok_or("a free route is leased")?;
    assert!(store.lease_route("rivethub#desktop").await?.is_none());
    assert!(store.lease_route("rivethub").await?.is_some());
    drop(lease);
    let freed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(lease) = store.lease_route("rivethub#desktop").await? {
                return Ok::<_, Box<dyn std::error::Error>>(lease);
            }
        }
    })
    .await??;
    drop(freed);
    Ok(())
}
