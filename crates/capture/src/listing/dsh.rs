use std::path::Path;

use kernel::transcript::StoreSession;
use kernel::transcript::TranscriptReaderError;
use wire::harness::HarnessId;

use super::files::file_times;

pub(crate) fn list(root: &Path, limit: usize) -> Result<Vec<StoreSession>, TranscriptReaderError> {
    let mut rows: Vec<StoreSession> = crate::readers::dsh::sessions(root)?
        .into_iter()
        .filter_map(|(id, path)| {
            let times = file_times(&path)?;
            let mut row = StoreSession::new(HarnessId::Dsh, &id, times.mtime);
            row.created_at = Some(times.birth_or_mtime());
            Some(row)
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.updated_at));
    rows.truncate(limit);
    Ok(rows)
}
