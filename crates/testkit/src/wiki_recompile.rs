use std::path::Path;

use jiff::Timestamp;
use kernel::memory::summary_reference::SummaryReferenceError;
use kernel::memory::{MemoryStore, SummaryStore};
use kernel::wiki::WikiLookup;
use kernel::wiki::builder::{WikiBuilderStore, WikiStoreError};
use kernel::wiki::consolidate::WikiConsolidateStore;
use kernel::wiki::page::bound::ARCHIVED_TITLE;
use kernel::wiki::page::{
    ArticleSection, WikiHistoryEntry, WikiPage, WikiSlug, parse_wiki_page, serialize_wiki_page,
};
use kernel::wiki::recompile::{
    RecompileReport, RecompileWiki, Recompiled, WikiRecompileStore, recompile,
};

use crate::topics::Outcome;
use crate::wiki_bounded::RecordingMerger;
use crate::wiki_files::write_leaf;

type Found<T> = Result<T, Box<dyn std::error::Error>>;

const PAGE: &str = "topics/acmekit.md";
const ARCHIVE: &str = "archive/acmekit/2026-10.1.md";
const QUIET_PAGE: &str = "topics/quiet.md";
const NOW: &str = "2026-10-30T12:00:00Z";

pub async fn restores_history_and_archives_copies(
    store: &(impl WikiRecompileStore + MemoryStore + WikiConsolidateStore),
    root: &Path,
) -> Outcome {
    git(root, &["init", "-q", "-b", "main", "--object-format=sha1"])?;
    git(root, &["config", "--local", "commit.gpgsign", "false"])?;
    git(root, &["config", "--local", "core.autocrlf", "false"])?;
    let slug = seed(root)?;
    let report = recompile(
        store,
        &RecordingMerger::default(),
        &RecompileWiki {
            slug: slug.clone(),
            since: "2026-10-01T00:00:00Z".parse()?,
        },
        NOW.parse()?,
    )
    .await?;
    assert_eq!(
        report,
        Some(RecompileReport {
            restored: 2,
            merges: 2,
            merge_failures: 0,
        })
    );
    let page = assert_restored(root)?;
    assert_row(store, root, &page, NOW.parse()?).await?;
    let stats = store.wiki_topic_stats().await?;
    let stat = stats
        .iter()
        .find(|row| row.slug == slug.as_ref())
        .ok_or("page stats")?;
    assert_eq!(stat.history, u64::try_from(page.history.len())?);
    assert_eq!(stat.created_at, NOW.parse::<Timestamp>()?);
    assert_archive(store, root, &slug).await?;
    assert_eq!(
        git(root, &["log", "-1", "--format=%B"])?,
        "wiki(acmekit): recompile to a bounded page\n\nRestored 2 history entries; 2 merge(s).\nPipeline: wiki-v7\n\n"
    );
    assert_eq!(git(root, &["rev-list", "--count", "HEAD"])?, "3\n");
    assert_eq!(
        [
            git(root, &["hash-object", "--no-filters", "--", PAGE])?,
            git(root, &["hash-object", "--no-filters", "--", ARCHIVE])?,
            git(root, &["rev-parse", "HEAD^{tree}"])?,
        ],
        [
            "a23813aadb50b63d854613e4312fdcb1025c33ce\n",
            "bd60e0e80946787a9d8ab9dc22016e08235c59ec\n",
            "b949d16cb9cbb83dfbd300afc3fa1c70260fb7db\n"
        ]
    );
    quiet_and_missing(store, root).await
}

fn seed(root: &Path) -> Found<WikiSlug> {
    let slug = WikiSlug::try_from("acmekit".to_owned())?;
    let mut page = WikiPage::empty(slug.clone(), "AcmeKit".to_owned());
    "AcmeKit ships on Fridays.".clone_into(&mut page.current_state);
    page.meta.aliases = vec!["making-acmekit".to_owned()];
    page.meta.tags = vec!["build".to_owned()];
    page.meta.entities = vec!["build-box".to_owned()];
    page.meta.related = vec!["quiet".to_owned()];
    page.history = vec![
        entry("2026-10-29", "Moved the build box", "Builds moved.")?,
        entry("2026-10-28", "First release", "Shipped 1.0.")?,
    ];
    let mut quiet = WikiPage::empty(WikiSlug::try_from("quiet".to_owned())?, "Quiet".to_owned());
    "A quiet page.".clone_into(&mut quiet.current_state);
    "The article remains as written.".clone_into(&mut quiet.article);
    std::fs::create_dir_all(root.join("topics"))?;
    std::fs::write(root.join(QUIET_PAGE), serialize_wiki_page(&quiet)?)?;
    commit_page(root, &page, "real history", "2026-10-29T12:00:00Z")?;
    page.history = (0..4)
        .map(|index| entry("2026-10-30", "Superseded article", &format!("copy {index}")))
        .collect::<Result<Vec<_>, _>>()?;
    page.extra_sections = vec![ArticleSection {
        heading: "Desktop".to_owned(),
        body: (0..80)
            .map(|index| format!("### Note {index}\n\n{index} {}", "y".repeat(120)))
            .collect::<Vec<_>>()
            .join("\n\n"),
    }];
    commit_page(root, &page, "copies pushed the history out", NOW)?;
    Ok(slug)
}

fn assert_restored(root: &Path) -> Found<WikiPage> {
    let page = parse_wiki_page(&std::fs::read_to_string(root.join(PAGE))?)?;
    let titles: Vec<&str> = page
        .history
        .iter()
        .map(|entry| entry.title.as_str())
        .collect();
    assert_eq!(
        titles,
        vec![ARCHIVED_TITLE, "Moved the build box", "First release"]
    );
    assert_eq!(page.current_state, "AcmeKit ships on Fridays. Merged.");
    Ok(page)
}

async fn assert_archive(store: &impl MemoryStore, root: &Path, slug: &WikiSlug) -> Outcome {
    let archive = std::fs::read_to_string(root.join(ARCHIVE))?;
    assert!(archive.starts_with("# Archive: AcmeKit, 2026-10"));
    for archived in ["copy 3", "### Note 16", "### Note 79"] {
        assert!(archive.contains(archived));
    }
    assert_eq!(
        store.wiki_archive(slug, Some("2026-10")).await?,
        Some(archive.clone())
    );
    assert_eq!(store.wiki_archive(slug, None).await?, Some(archive));
    assert_eq!(store.wiki_archive(slug, Some("2026-09")).await?, None);
    assert_eq!(store.wiki_archive(slug, Some("../x")).await?, None);
    Ok(())
}

async fn assert_row(
    store: &impl MemoryStore,
    root: &Path,
    page: &WikiPage,
    now: Timestamp,
) -> Outcome {
    let WikiLookup::Page(row) = store.wiki_page(&page.meta.slug).await? else {
        return Err("the recompiled page row is missing".into());
    };
    let head = git(root, &["rev-parse", "HEAD"])?;
    assert_eq!(row.git_sha.as_deref(), Some(head.trim()));
    assert_eq!(row.slug, page.meta.slug.as_ref());
    assert_eq!(row.title, page.meta.title);
    assert_eq!(row.aliases, page.meta.aliases);
    assert_eq!(row.tags, page.meta.tags);
    assert_eq!(row.entities, page.meta.entities);
    assert_eq!(row.related.as_ref(), Some(&page.meta.related));
    assert_eq!(row.current_state, page.current_state);
    assert_eq!(
        row.article.as_deref(),
        (!page.article.is_empty()).then_some(page.article.as_str())
    );
    assert_eq!(row.updated_at, now);
    assert_eq!(row.last_verified, Some(now));
    Ok(())
}

async fn quiet_and_missing(
    store: &(impl WikiRecompileStore + MemoryStore),
    root: &Path,
) -> Outcome {
    let quiet = RecompileWiki {
        slug: WikiSlug::try_from("quiet".to_owned())?,
        since: "2026-10-31T00:00:00Z".parse()?,
    };
    let now = "2026-11-01T12:00:00Z".parse()?;
    let merger = RecordingMerger::default();
    let before = git(root, &["rev-parse", "HEAD"])?;
    let bytes = std::fs::read_to_string(root.join(QUIET_PAGE))?;
    assert_eq!(
        recompile(store, &merger, &quiet, now).await?,
        Some(RecompileReport::default())
    );
    assert_eq!(std::fs::read_to_string(root.join(QUIET_PAGE))?, bytes);
    assert_eq!(store.wiki_archive(&quiet.slug, None).await?, None);
    assert_row(store, root, &parse_wiki_page(&bytes)?, now).await?;
    let missing = RecompileWiki {
        slug: WikiSlug::try_from("missing".to_owned())?,
        since: quiet.since,
    };
    assert_eq!(recompile(store, &merger, &missing, now).await?, None);
    assert!(matches!(
        store.wiki_page(&missing.slug).await?,
        WikiLookup::Missing
    ));
    assert_eq!(merger.seen.lock().len(), 0);
    assert_eq!(git(root, &["rev-parse", "HEAD"])?, before);
    Ok(())
}

pub async fn validates_cited_summaries(
    store: &(impl WikiRecompileStore + WikiBuilderStore + SummaryStore + MemoryStore),
    root: &Path,
) -> Outcome {
    let slug = WikiSlug::try_from("quiet".to_owned())?;
    let now = NOW.parse()?;
    let id = "00000000-0000-4000-8000-0000000000d1";
    let content = "AcmeKit publishes release notes on Fridays.";
    write_leaf(store, id, content, NOW).await?;
    let id = uuid::Uuid::parse_str(id)?;
    store.cite_wiki(id, &slug, now).await?;
    let job = RecompileWiki { slug, since: now };
    let source = store
        .recompile_source(&job.slug, job.since)
        .await?
        .ok_or("quiet page")?;
    let summary = source.summaries.first().ok_or("cited summary")?;
    assert_eq!(summary.content, content);
    assert_eq!(summary.at, now);
    let mut stale = summary.input.clone().ok_or("summary input")?;
    assert_eq!(stale.id, id);
    "An outdated summary.".clone_into(&mut stale.content);
    let before = git(root, &["rev-parse", "HEAD"])?;
    let bytes = std::fs::read(root.join(QUIET_PAGE))?;
    let mut page = source.page.clone();
    "A stale recompile.".clone_into(&mut page.current_state);
    let result = store
        .store_recompiled(
            Recompiled {
                inputs: vec![stale],
                base: source.page,
                page,
                archived: Vec::new(),
                message: "stale recompile".to_owned(),
            },
            now,
        )
        .await;
    assert!(
        matches!(result, Err(WikiStoreError::Reference(SummaryReferenceError::Stale(found))) if found == id)
    );
    assert_eq!(git(root, &["rev-parse", "HEAD"])?, before);
    assert_eq!(std::fs::read(root.join(QUIET_PAGE))?, bytes);

    let merger = RecordingMerger::default();
    let report = recompile(store, &merger, &job, now)
        .await?
        .ok_or("quiet page")?;
    assert_eq!(report.merges, 2);
    let page = parse_wiki_page(&std::fs::read_to_string(root.join(QUIET_PAGE))?)?;
    assert!(page.article.contains(content));
    assert_row(store, root, &page, now).await
}

fn entry(date: &str, title: &str, body: &str) -> Result<WikiHistoryEntry, jiff::Error> {
    Ok(WikiHistoryEntry {
        date: date.parse()?,
        title: title.to_owned(),
        body: body.to_owned(),
    })
}

fn commit_page(root: &Path, page: &WikiPage, message: &str, date: &str) -> Outcome {
    std::fs::write(root.join(PAGE), serialize_wiki_page(page)?)?;
    git(root, &["add", "--", "topics"])?;
    let mut commit = git_command(root);
    commit
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date);
    git_output(commit.args(["commit", "-q", "-m", message]))?;
    Ok(())
}

fn git_command(root: &Path) -> std::process::Command {
    let mut git = std::process::Command::new("git");
    git.current_dir(root)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"]);
    git
}

fn git(root: &Path, args: &[&str]) -> Found<String> {
    git_output(git_command(root).args(args))
}

fn git_output(command: &mut std::process::Command) -> Found<String> {
    let output = command.output()?;
    if output.status.success() {
        Ok(String::from_utf8(output.stdout)?)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into())
    }
}
