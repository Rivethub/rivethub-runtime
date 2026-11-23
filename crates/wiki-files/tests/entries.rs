use std::path::Path;

use jiff::Timestamp;
use kernel::wiki::page::WikiHistoryEntry;
use kernel::wiki::page::WikiPage;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::bound::Archived;
use wiki_files::wiki_writer::WikiRepository;
use wiki_files::wiki_writer::archive::archive_parts;
use wire::time::TimestampFormat as _;

#[tokio::test]
async fn read_unlocked_history_since_and_recompile_round_trip() -> Result<(), EntryError> {
    let dir = tempfile::tempdir()?;
    let root = std::fs::canonicalize(dir.path())?;
    let slug = WikiSlug::try_from("bench".to_owned())?;
    let repo = WikiRepository::for_store(root.clone());
    let at = stamp("2026-10-10T12:00:00Z")?;
    let mut page = WikiPage::empty(slug.clone(), "Bench".to_owned());
    page.current_state = "Ready.".to_owned();
    page.article = "The bench faces the window.".to_owned();
    page.history.push(WikiHistoryEntry {
        date: at.utc_date(),
        title: "Noted".to_owned(),
        body: "The first note.".to_owned(),
    });
    record(&repo, &mut page, at).await?;
    let read = repo
        .read_unlocked(&slug)
        .await?
        .ok_or("the recorded page is missing")?;
    assert_eq!(read.article, "The bench faces the window.");
    let history = repo
        .history_since(&slug, stamp("2026-10-10T11:00:00Z")?)
        .await?;
    assert!(
        history.iter().any(|entry| entry.body == "The first note."),
        "{history:?}"
    );
    let sha = recompile(&repo, &mut page).await?;
    assert_eq!(sha, head(&root)?);
    let archived = archive_text(&root)?;
    assert!(archived.contains("part-two, part-ten"), "{archived}");
    Ok(())
}

#[expect(clippy::significant_drop_tightening)]
async fn record(
    repo: &WikiRepository,
    page: &mut WikiPage,
    at: jiff::Timestamp,
) -> Result<(), EntryError> {
    let mut locked = repo.lock().await?;
    locked.fold(page, &[], "wiki(bench): record", at).await?;
    locked.release().await?;
    Ok(())
}

#[expect(clippy::significant_drop_tightening)]
async fn recompile(repo: &WikiRepository, page: &mut WikiPage) -> Result<String, EntryError> {
    let mut locked = repo.lock().await?;
    let sha = locked
        .recompile(
            page,
            vec![Archived {
                label: "Entities".to_owned(),
                text: "part-two, part-ten".to_owned(),
                notice: false,
            }],
            "wiki(bench): recompile",
            stamp("2026-10-10T13:00:00Z")?,
        )
        .await?;
    locked.release().await?;
    Ok(sha)
}

#[tokio::test]
#[expect(clippy::significant_drop_tightening)]
async fn a_caller_timestamp_does_not_hide_the_revision() -> Result<(), EntryError> {
    let dir = tempfile::tempdir()?;
    let root = std::fs::canonicalize(dir.path())?;
    let slug = WikiSlug::try_from("bench".to_owned())?;
    let repo = WikiRepository::for_store(root.clone());
    let january = stamp("2026-01-15T12:00:00Z")?;
    let mut page = WikiPage::empty(slug.clone(), "Bench".to_owned());
    page.current_state = "Ready.".to_owned();
    page.history.push(WikiHistoryEntry {
        date: january.utc_date(),
        title: "Noted".to_owned(),
        body: "January note.".to_owned(),
    });
    record(&repo, &mut page, january).await?;
    let mut locked = repo.lock().await?;
    locked
        .remove(
            &[slug.as_ref().to_owned()],
            "wiki(bench): removed after a tidy review",
        )
        .await?;
    locked.release().await?;
    let history = repo
        .history_since(&slug, stamp("2025-11-01T00:00:00Z")?)
        .await?;
    assert_eq!(
        history
            .iter()
            .filter(|entry| entry.body == "January note.")
            .count(),
        1,
        "{history:?}"
    );
    let month = Timestamp::now().strftime("%Y-%m").to_string();
    let dated = committer_months(&root)?;
    assert_eq!(dated.len(), 2);
    for date in dated {
        assert!(date.starts_with(&month), "{date} is not {month}");
    }
    Ok(())
}

#[tokio::test]
#[expect(clippy::significant_drop_tightening)]
async fn a_repository_keeps_its_own_git_settings() -> Result<(), EntryError> {
    let dir = tempfile::tempdir()?;
    let root = std::fs::canonicalize(dir.path())?;
    let repo = WikiRepository::for_store(root.clone());
    let locked = repo.lock().await?;
    assert_eq!(locked.read("bench").await?, None);
    locked.release().await?;
    set_config(&root, "commit.gpgsign", "true")?;
    set_config(&root, "core.autocrlf", "true")?;
    let locked = repo.lock().await?;
    assert_eq!(locked.read("bench").await?, None);
    locked.release().await?;
    assert_eq!(config(&root, "commit.gpgsign")?, "true");
    assert_eq!(config(&root, "core.autocrlf")?, "true");
    assert_eq!(config(&root, "user.name")?, "RivetOS Wiki");
    assert_eq!(config(&root, "user.email")?, "wiki@rivetos.dev");
    Ok(())
}

#[cfg(feature = "git-fault")]
#[tokio::test]
#[expect(clippy::significant_drop_tightening)]
async fn an_inner_fault_guard_does_not_clear_the_outer_one() -> Result<(), EntryError> {
    let dir = tempfile::tempdir()?;
    let root = std::fs::canonicalize(dir.path())?;
    let outer = wiki_files::wiki_writer::fail_git(&root, "rev-parse");
    {
        let _inner = wiki_files::wiki_writer::fail_git(&root, "status");
    }
    let repo = WikiRepository::for_store(root);
    let locked = repo.lock().await?;
    let read = locked.read("bench").await;
    let Err(error) = read else {
        return Err("the outer git fault was cleared".into());
    };
    let rendered = format!("{error}");
    assert!(rendered.contains("injected git failure"), "{rendered}");
    drop(outer);
    Ok(())
}

#[tokio::test]
async fn archive_parts_sort_part_two_before_part_ten() -> Result<(), EntryError> {
    let dir = tempfile::tempdir()?;
    let directory = dir.path().join("archive").join("bench");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("2026-10.10.md"), "ten")?;
    std::fs::write(directory.join("2026-10.2.md"), "two")?;
    let parts = archive_parts(&directory).await?;
    let names: Vec<_> = parts.iter().map(|(part, _)| part.part).collect();
    assert_eq!(names, vec![2, 10]);
    Ok(())
}

type EntryError = Box<dyn std::error::Error>;

fn set_config(root: &Path, key: &str, value: &str) -> Result<(), EntryError> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--local", key, value])
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git config {key} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    }
}

fn config(root: &Path, key: &str) -> Result<String, EntryError> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--local", "--get", key])
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn stamp(text: &str) -> Result<Timestamp, EntryError> {
    Ok(text.parse()?)
}

fn committer_months(root: &Path) -> Result<Vec<String>, EntryError> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "--format=%cI"])
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into());
    }
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .map(str::to_owned)
        .collect())
}

fn head(root: &Path) -> Result<String, EntryError> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn archive_text(root: &Path) -> Result<String, EntryError> {
    let mut text = String::new();
    let archive = root.join("archive");
    if !archive.exists() {
        return Ok(text);
    }
    for entry in std::fs::read_dir(archive)? {
        let entry = entry?;
        if entry.path().is_dir() {
            for file in std::fs::read_dir(entry.path())? {
                text.push_str(&std::fs::read_to_string(file?.path())?);
            }
        }
    }
    Ok(text)
}
