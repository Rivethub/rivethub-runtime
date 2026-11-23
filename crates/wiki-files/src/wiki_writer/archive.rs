use jiff::Timestamp;
use std::path::{Path, PathBuf};

use kernel::wiki::builder::WikiWriteError;
use kernel::wiki::page::WikiPage;
use kernel::wiki::page::archive::{
    ARCHIVE_DIRECTORY, ArchivePart, archive_directory, archive_header, archive_month, archive_path,
    next_part, settle_archive, unarchived,
};
use kernel::wiki::page::bound::{Archived, bound_page};
use tokio::io::AsyncWriteExt as _;

use super::{git, reject_symlink};

pub(super) async fn bound(
    root: &Path,
    previous: Option<&WikiPage>,
    page: &mut WikiPage,
    at: Timestamp,
    extra: Vec<Archived>,
) -> Result<Vec<String>, WikiWriteError> {
    let mut archived = extra;
    archived.extend(bound_page(previous, page));
    if archived.is_empty() {
        return Ok(Vec::new());
    }
    let slug = page.meta.slug.as_ref().to_owned();
    let month = archive_month(at);
    let directory = root.join(archive_directory(&slug));
    reject_symlink(&root.join(ARCHIVE_DIRECTORY)).await?;
    reject_symlink(&directory).await?;
    let mut stored = String::new();
    let mut sizes = Vec::new();
    for (part, path) in archive_parts(&directory).await? {
        if part.month != month {
            continue;
        }
        reject_symlink(&path).await?;
        let text = tokio::fs::read_to_string(&path).await?;
        sizes.push((part, text.len()));
        stored.push_str(&super::references::archive(text)?);
    }
    let mut archived = unarchived(archived, page, &stored);
    let inline = head(root).await;
    let Some(block) = settle_archive(page, at, inline.as_deref(), &mut archived) else {
        return Ok(Vec::new());
    };
    let header = sizes
        .is_empty()
        .then(|| archive_header(&page.meta.title, &slug, &month));
    let relative = archive_path(&slug, &next_part(&month, &sizes, block.len()));
    reject_symlink(&root.join(&relative)).await?;
    append(root, &relative, header.as_deref(), &block).await?;
    Ok(vec![relative])
}

pub async fn archive_parts(
    directory: &Path,
) -> Result<Vec<(ArchivePart, PathBuf)>, std::io::Error> {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut parts = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let part = entry.file_name().to_str().and_then(ArchivePart::parse);
        parts.extend(part.map(|part| (part, entry.path())));
    }
    parts.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(parts)
}

async fn head(root: &Path) -> Option<String> {
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let sha = git(root, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .await
        .ok();
    sha.map(|sha| sha.trim().to_owned())
        .filter(|sha| !sha.is_empty())
}

async fn append(
    root: &Path,
    relative: &str,
    header: Option<&str>,
    block: &str,
) -> Result<(), WikiWriteError> {
    let destination = root.join(relative);
    let Some(directory) = destination.parent() else {
        return Err(WikiWriteError::Containment(destination.clone()));
    };
    tokio::fs::create_dir_all(directory).await?;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&destination)
        .await?;
    if let Some(header) = header {
        file.write_all(header.as_bytes()).await?;
    }
    file.write_all(block.as_bytes()).await?;
    file.sync_all().await?;
    drop(file);
    git(root, &["add", "--", relative]).await?;
    Ok(())
}
