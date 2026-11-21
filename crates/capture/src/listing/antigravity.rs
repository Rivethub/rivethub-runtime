use std::path::PathBuf;

use kernel::transcript::StoreSession;
use kernel::transcript::TranscriptReaderError;
use wire::harness::HarnessId;

use super::files::file_times;
use crate::readers::antigravity;

pub(crate) fn list(
    roots: &[PathBuf],
    limit: usize,
) -> Result<Vec<StoreSession>, TranscriptReaderError> {
    let mut rows: Vec<StoreSession> = antigravity::sessions(roots)?
        .into_iter()
        .filter_map(|(id, app_dir, path)| {
            let times = file_times(&path)?;
            let summary = antigravity::summary_in(&app_dir, &id);
            let mut row = StoreSession::new(HarnessId::Antigravity, &id, times.mtime);
            row.created_at = Some(times.birth_or_mtime());
            if let Some(title) = summary.title {
                row.title = title;
            }
            row.cwd = summary.cwd;
            row.parent_session_id = summary.parent_conversation_id;
            Some(row)
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.updated_at));
    rows.truncate(limit);
    Ok(rows)
}

pub(crate) fn describe(
    roots: &[PathBuf],
    native: &str,
) -> Result<Option<StoreSession>, TranscriptReaderError> {
    for app_dir in roots {
        let Some(path) = antigravity::transcript_path_for(app_dir, native)? else {
            continue;
        };
        let Some(times) = file_times(&path) else {
            continue;
        };
        let summary = antigravity::summary_in(app_dir, native);
        let mut row = StoreSession::new(HarnessId::Antigravity, native, times.mtime);
        row.created_at = Some(times.birth_or_mtime());
        if let Some(title) = summary.title {
            row.title = title;
        }
        row.cwd = summary.cwd;
        row.parent_session_id = summary.parent_conversation_id;
        return Ok(Some(row));
    }
    Ok(None)
}
