use std::path::Path;

use kernel::wiki::page::archive::{
    ARCHIVE_DIRECTORY, archive_directory, legacy_archive_path, parse_archive_month,
};
use wiki_files::wiki_writer::archive::archive_parts;

use crate::SqliteError;

pub(crate) async fn read(
    root: &Path,
    slug: &str,
    month: Option<&str>,
) -> Result<Option<String>, SqliteError> {
    let root = match tokio::fs::canonicalize(root).await {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SqliteError::WikiIo(error)),
    };
    let month = match month {
        Some(month) => match parse_archive_month(month) {
            Some(month) => Some(month),
            None => return Ok(None),
        },
        None => None,
    };
    let archives = root.join(ARCHIVE_DIRECTORY);
    let mut paths = Vec::new();
    if month.is_none() {
        paths.push(root.join(legacy_archive_path(slug)));
    }
    let parts = archive_parts(&root.join(archive_directory(slug)))
        .await
        .map_err(SqliteError::WikiIo)?;
    paths.extend(
        parts
            .into_iter()
            .filter(|(part, _)| month.as_ref().is_none_or(|month| part.month == *month))
            .map(|(_, path)| path),
    );
    let mut found = None;
    for path in paths {
        let canonical = match tokio::fs::canonicalize(&path).await {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(SqliteError::WikiIo(error)),
        };
        if !canonical.starts_with(&archives) {
            return Err(SqliteError::Invalid {
                message: "wiki archive escaped its directory".to_owned(),
            });
        }
        let text = tokio::fs::read_to_string(canonical)
            .await
            .map_err(SqliteError::WikiIo)?;
        let text = wiki_files::wiki_writer::references::archive(text).map_err(|error| {
            SqliteError::Invalid {
                message: error.to_string(),
            }
        })?;
        found.get_or_insert_with(String::new).push_str(&text);
    }
    Ok(found)
}
