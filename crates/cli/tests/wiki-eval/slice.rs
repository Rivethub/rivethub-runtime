use std::path::Path;

use uuid::Uuid;

use crate::failure::Failure;

pub(crate) fn load(path: &Path) -> Result<Vec<Uuid>, Failure> {
    let text = std::fs::read_to_string(path).map_err(|source| Failure::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut ids = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let id = Uuid::parse_str(trimmed).map_err(|_| Failure::Slice {
            path: path.to_path_buf(),
            detail: format!("line {} is not a uuid", index.saturating_add(1)),
        })?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Err(Failure::Slice {
            path: path.to_path_buf(),
            detail: "names no conversations".to_owned(),
        });
    }
    Ok(ids)
}

pub(crate) fn label_ok(label: &str) -> bool {
    !label.is_empty()
        && label
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}
