use std::path::Path;
use std::process::Stdio;

use chrono::{DateTime, Utc};
use kernel::wiki::builder::WikiWriteError;
use kernel::wiki::page::{WikiHistoryEntry, WikiSlug, parse_wiki_page};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::Command;

use super::{git, topic_path};

pub(super) async fn since(
    root: &Path,
    slug: &WikiSlug,
    since: DateTime<Utc>,
) -> Result<Vec<WikiHistoryEntry>, WikiWriteError> {
    let path = topic_path(slug);
    let after = format!("--since={}", since.to_rfc3339());
    let before = format!("--before={}", since.to_rfc3339());
    let mut commits: Vec<String> = git(root, &["rev-list", &after, "HEAD", "--", &path])
        .await?
        .lines()
        .map(str::to_owned)
        .collect();
    if let Some(first) = git(root, &["rev-list", "-1", &before, "HEAD", "--", &path])
        .await?
        .lines()
        .next()
    {
        commits.push(first.to_owned());
    }
    let specs: Vec<String> = commits
        .iter()
        .map(|commit| format!("{commit}:{path}"))
        .collect();
    let mut entries: Vec<WikiHistoryEntry> = Vec::new();
    blobs(root, &specs, |markdown| {
        #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
        let Ok(page) = parse_wiki_page(markdown) else {
            return;
        };
        for entry in page.history {
            if !entries.contains(&entry) {
                entries.push(entry);
            }
        }
    })
    .await?;
    Ok(entries)
}

async fn blobs(
    root: &Path,
    specs: &[String],
    mut each: impl FnMut(&str),
) -> Result<(), WikiWriteError> {
    let mut child = Command::new("git")
        .current_dir(root)
        .args(["cat-file", "--batch"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or_else(missing_pipe)?;
    let stdout = child.stdout.take().ok_or_else(missing_pipe)?;
    let input = specs.join("\n") + "\n";
    let writer = tokio::spawn(async move {
        stdin.write_all(input.as_bytes()).await?;
        stdin.shutdown().await
    });
    let mut reader = BufReader::new(stdout);
    for _ in specs {
        let mut header = String::new();
        if reader.read_line(&mut header).await? == 0 {
            break;
        }
        let mut parts = header.split_whitespace();
        let (Some(_), Some("blob"), Some(size)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let size: usize = size.parse().map_err(|_| missing_pipe())?;
        let mut body = vec![0u8; size.saturating_add(1)];
        reader.read_exact(&mut body).await?;
        body.pop();
        each(&String::from_utf8(body)?);
    }
    writer.await??;
    child.wait().await?;
    Ok(())
}

fn missing_pipe() -> WikiWriteError {
    WikiWriteError::Io(std::io::Error::other("git cat-file pipe"))
}
