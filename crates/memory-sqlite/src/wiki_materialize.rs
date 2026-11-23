use std::path::Path;

use jiff::Timestamp;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::page::WikiSlug;
use rusqlite::Connection;
use wiki_files::wiki_writer::LockedRepository;

use crate::SqliteError;

pub(crate) async fn record_stored(
    locked: &mut LockedRepository,
    conn: &Connection,
    slug: &WikiSlug,
    now: Timestamp,
) -> Result<(), WikiStoreError> {
    let Some(stored) =
        crate::wiki_read::database_page(conn, slug.as_ref()).map_err(stored_error)?
    else {
        return Ok(());
    };
    if stored.git_sha.is_some() {
        return Ok(());
    }
    if differs(locked.root(), slug)? {
        return Err(WikiStoreError::InvalidPatch(format!(
            "topics/{}.md differs from HEAD",
            slug.as_ref()
        )));
    }
    let message = format!(
        "wiki({}): record the stored page\n\nPipeline: wiki-v7",
        slug.as_ref()
    );
    let mut page = stored.page;
    locked
        .fold(&mut page, &[], &message, now)
        .await
        .map_err(WikiStoreError::from)?;
    Ok(())
}

fn differs(root: &Path, slug: &WikiSlug) -> Result<bool, WikiStoreError> {
    let relative = format!("topics/{}.md", slug.as_ref());
    let bytes = match std::fs::read(root.join(&relative)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(WikiStoreError::Query(Box::new(error))),
    };
    Ok(committed(root, &relative)?.as_deref() != Some(bytes.as_slice()))
}

fn committed(root: &Path, relative: &str) -> Result<Option<Vec<u8>>, WikiStoreError> {
    let spec = format!("HEAD:{relative}");
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(["show", &spec])
        .output()
        .map_err(|error| WikiStoreError::Query(Box::new(error)))?;
    if output.status.success() {
        return Ok(Some(output.stdout));
    }
    Ok(None)
}

fn stored_error(error: SqliteError) -> WikiStoreError {
    WikiStoreError::Query(Box::new(error))
}
