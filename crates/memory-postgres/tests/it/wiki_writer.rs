#![cfg(test)]

use jiff::Timestamp;
use std::path::Path;
use std::time::Duration;
use wire::time::TimestampFormat as _;

use kernel::wiki::builder::WikiWriteError;
use kernel::wiki::page::{
    CitationKind, PageCitation, PagePatch, PageSource, WikiAction, normalize_slug, parse_wiki_page,
};
use kernel::wiki::page::{WikiHistoryEntry, WikiSourceKind};
use memory_postgres::wiki_writer::{AppliedPage, WikiRepository};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn now() -> Timestamp {
    "2026-10-10T12:00:00Z".parse().expect("date")
}

const SUMMARY_ID: &str = "00000000-0000-4000-8000-000000000001";

fn patch(slug: &str) -> PagePatch {
    PagePatch {
        action: WikiAction::Create,
        slug: slug.to_owned(),
        verified_at: now(),
        title: Some("Deployment notes".to_owned()),
        add_aliases: Vec::new(),
        add_tags: vec!["deploy".to_owned()],
        add_entities: vec!["AcmeKit".to_owned()],
        add_related: Vec::new(),
        summary_delta: Some("Ship 🚀 safely.".to_owned()),
        current_state: None,
        article: None,
        article_patches: Vec::new(),
        history_entry: Some(WikiHistoryEntry {
            date: now().utc_date(),
            title: "Release".to_owned(),
            body: "Checked health.".to_owned(),
        }),
        add_citations: vec![PageCitation {
            summary_id: SUMMARY_ID.to_owned(),
            date: Some(now().utc_date()),
            kind: Some(CitationKind::Leaf),
            note: Some("Release | notes".to_owned()),
        }],
        add_sources: vec![PageSource {
            kind: WikiSourceKind::Summary,
            ids: vec![SUMMARY_ID.to_owned()],
            conversation_id: None,
            extra: wire::Extra::default(),
        }],
        allow_shrink: false,
    }
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

#[tokio::test]
async fn first_apply_commits_exact_identity_message_and_noop_returns_head() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("wiki");
    let first = apply(&root, patch("release")).await?;
    assert_eq!(first.git_sha.len(), 40);
    assert_eq!(
        git(&root, &["log", "-1", "--format=%B"]).await?,
        "wiki(release): create from summary 00000000\n\nProvenance: summary 00000000-0000-4000-8000-000000000001\nPipeline: wiki-v7\n\n"
    );
    assert_eq!(
        git(&root, &["log", "-1", "--format=%an <%ae>|%cn <%ce>"]).await?,
        "RivetOS Wiki <wiki@rivetos.dev>|RivetOS Wiki <wiki@rivetos.dev>\n"
    );
    assert_eq!(git(&root, &["branch", "--show-current"]).await?, "main\n");
    let second = apply(&root, patch("release")).await?;
    assert_eq!(first.git_sha, second.git_sha);
    assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]).await?, "1\n");
    assert!(!root.join(".wiki-writer.lock").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_owners_on_one_root_produce_two_serial_commits() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let (first, second) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::try_join!(apply(root, patch("one")), apply(root, patch("two")))
    })
    .await??;
    assert_ne!(first.git_sha, second.git_sha);
    assert_eq!(git(root, &["rev-list", "--count", "HEAD"]).await?, "2\n");
    assert_eq!(
        git(root, &["status", "--porcelain", "--", "topics"]).await?,
        ""
    );
    Ok(())
}

#[tokio::test]
async fn stale_lock_is_reclaimed_and_foreign_replacement_is_not_unlinked() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let path = root.join(".wiki-writer.lock");
    tokio::fs::write(&path, "foreign 1\n").await?;
    let released = {
        let repository = WikiRepository::new(root.to_path_buf()).lock().await?;
        let owned = tokio::fs::read_to_string(&path).await?;
        let token = owned.split_whitespace().next().ok_or("token")?;
        let (pid, id) = token.split_once('-').ok_or("pid prefix")?;
        assert_eq!(pid.parse::<u32>()?, std::process::id());
        assert_eq!(uuid::Uuid::parse_str(id)?.get_version_num(), 4);
        assert_eq!(
            tokio::fs::metadata(&path).await?.permissions().mode() & 0o777,
            0o600
        );
        tokio::fs::write(&path, "replacement 9999999999999\n").await?;
        repository.release().await
    };
    released?;
    assert_eq!(
        tokio::fs::read_to_string(path).await?,
        "replacement 9999999999999\n"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn live_foreign_lock_is_waited_on_then_released() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join(".wiki-writer.lock");
    tokio::fs::write(
        &path,
        format!("foreign {}\n", Timestamp::now().epoch_millis()),
    )
    .await?;
    let repository = WikiRepository::new(directory.path().to_path_buf());
    let mut pending = Box::pin(repository.lock());
    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut pending)
            .await
            .is_err()
    );
    assert!(path.exists());
    tokio::fs::remove_file(path).await?;
    tokio::time::timeout(Duration::from_secs(5), pending)
        .await??
        .release()
        .await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn foreign_lock_exhaustion_is_typed() -> TestResult {
    let directory = tempfile::tempdir()?;
    tokio::fs::write(
        directory.path().join(".wiki-writer.lock"),
        "foreign 9999999999999\n",
    )
    .await?;
    let result = WikiRepository::new(directory.path().to_path_buf())
        .lock()
        .await;
    assert!(matches!(result, Err(WikiWriteError::LockTimeout)));
    drop(result);
    Ok(())
}

#[tokio::test]
async fn fixture_keeps_history_article_frontmatter_and_sources_through_apply() -> TestResult {
    let directory = tempfile::tempdir()?;
    tokio::fs::create_dir(directory.path().join("topics")).await?;
    let fixture = tokio::fs::read_to_string(testkit::recording(
        "mcp-memory/shared/wiki/topics/kiwi-build-cache.txt",
    ))
    .await?;
    let before = parse_wiki_page(&fixture)?;
    tokio::fs::write(directory.path().join("topics/kiwi-build-cache.md"), fixture).await?;
    let applied = apply(directory.path(), patch("kiwi-build-cache")).await?;
    assert_eq!(applied.page.article, before.article);
    assert_eq!(applied.page.history.len(), before.history.len() + 1);
    assert_eq!(applied.page.citations.len(), before.citations.len() + 1);
    assert!(
        applied
            .page
            .meta
            .aliases
            .contains(&"kiwi sccache".to_owned())
    );
    let text =
        tokio::fs::read_to_string(directory.path().join("topics/kiwi-build-cache.md")).await?;
    assert_eq!(parse_wiki_page(&text)?, applied.page);
    Ok(())
}

#[tokio::test]
async fn containment_refuses_symlinks_parent_paths_and_accepts_quoted_slug() -> TestResult {
    let directory = tempfile::tempdir()?;
    let sibling = tempfile::tempdir()?;
    tokio::fs::create_dir(directory.path().join("topics")).await?;
    let outside = sibling.path().join("untouched.md");
    tokio::fs::write(&outside, "untouched").await?;
    tokio::fs::symlink(&outside, directory.path().join("topics/escape.md")).await?;
    assert!(matches!(
        apply(directory.path(), patch("escape")).await,
        Err(WikiWriteError::Containment(_))
    ));
    assert_eq!(tokio::fs::read_to_string(&outside).await?, "untouched");
    assert!(matches!(
        WikiRepository::new(directory.path().join("../escape"))
            .lock()
            .await,
        Err(WikiWriteError::Containment(_))
    ));
    apply(directory.path(), patch("developer's-notes")).await?;
    Ok(())
}

#[tokio::test]
async fn quoted_glob_slug_is_normalized_and_unrelated_staged_work_is_preserved() -> TestResult {
    let directory = tempfile::tempdir()?;
    apply(directory.path(), patch("release")).await?;
    tokio::fs::write(directory.path().join("human.txt"), "human work").await?;
    git(directory.path(), &["add", "human.txt"]).await?;
    let slug = "Acme's $(topic) [draft]* 日本語";
    apply(directory.path(), patch(slug)).await?;
    let names = git(
        directory.path(),
        &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"],
    )
    .await?;
    assert!(!names.contains("human.txt"));
    assert!(
        directory
            .path()
            .join("topics")
            .join(format!("{}.md", normalize_slug(slug)))
            .is_file()
    );
    assert_eq!(
        git(directory.path(), &["diff", "--cached", "--name-only"]).await?,
        "human.txt\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_repository_without_a_remote_commits_without_pulling() -> TestResult {
    let directory = tempfile::tempdir()?;
    apply(directory.path(), patch("release")).await?;
    tokio::fs::create_dir_all(directory.path().join(".git/refs/remotes")).await?;
    apply(directory.path(), patch("second")).await?;
    assert_eq!(
        git(directory.path(), &["rev-list", "--count", "HEAD"]).await?,
        "2\n"
    );
    Ok(())
}

#[tokio::test]
async fn pulls_only_while_the_branch_tracks_a_remote() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    git(root, &["init", "-q", "--bare", "-b", "main", "origin.git"]).await?;
    git(root, &["clone", "-q", "origin.git", "other"]).await?;
    upstream_commit(&root.join("other"), "first").await?;
    git(root, &["clone", "-q", "origin.git", "wiki"]).await?;
    let wiki = root.join("wiki");
    upstream_commit(&root.join("other"), "second").await?;
    apply(&wiki, patch("release")).await?;
    assert!(
        git(&wiki, &["log", "--format=%s"])
            .await?
            .contains("second")
    );
    git(&wiki, &["branch", "--unset-upstream"]).await?;
    upstream_commit(&root.join("other"), "third").await?;
    apply(&wiki, patch("later")).await?;
    let fetched = git(&wiki, &["log", "--format=%s", "origin/main"]).await?;
    assert!(!fetched.contains("third"), "{fetched}");
    Ok(())
}

async fn upstream_commit(clone: &Path, message: &str) -> TestResult {
    git(
        clone,
        &[
            "-c",
            "user.name=Upstream",
            "-c",
            "user.email=upstream@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            message,
        ],
    )
    .await?;
    git(clone, &["push", "-q", "origin", "HEAD:main"]).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn incomplete_foreign_lock_does_not_authorize_reclamation() -> TestResult {
    let directory = tempfile::tempdir()?;
    let filename = directory.path().join(".wiki-writer.lock");
    tokio::fs::write(&filename, "foreign invalid\n").await?;
    assert!(matches!(
        WikiRepository::new(directory.path().to_path_buf())
            .lock()
            .await,
        Err(WikiWriteError::LockTimeout)
    ));
    assert_eq!(
        tokio::fs::read_to_string(filename).await?,
        "foreign invalid\n"
    );
    Ok(())
}

#[tokio::test]
async fn independent_roots_do_not_share_the_process_owner() -> TestResult {
    let first = tempfile::tempdir()?;
    let second = tempfile::tempdir()?;
    let released = {
        let held = WikiRepository::new(first.path().to_path_buf())
            .lock()
            .await?;
        tokio::time::timeout(
            Duration::from_secs(5),
            apply(second.path(), patch("release")),
        )
        .await??;
        held.release().await
    };
    released?;
    Ok(())
}
