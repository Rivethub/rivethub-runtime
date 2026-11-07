use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;
use kernel::harness_setup::ConnectOutcome;
use kernel::harness_setup::SetupError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileEdit {
    pub(super) path: PathBuf,
    pub(super) before: Option<String>,
    pub(super) after: Option<String>,
}

impl FileEdit {
    #[must_use]
    pub(super) fn is_change(&self) -> bool {
        self.before != self.after
    }
}

#[derive(Debug, Clone)]
pub(super) struct Backups {
    directory: PathBuf,
}

impl Backups {
    #[must_use]
    pub(super) fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    fn path_for(&self, id: &str, source: &Path, now: DateTime<Utc>) -> PathBuf {
        let name = source.file_name().map_or_else(
            || "config".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        self.directory
            .join(format!("{id}-{name}-{}", now.format("%Y%m%dT%H%M%S%3fZ")))
    }
}

fn io_error(path: &Path, source: io::Error) -> SetupError {
    SetupError::Io {
        path: path.display().to_string(),
        source,
    }
}

pub(super) fn read_optional(path: &Path) -> Result<Option<String>, SetupError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(path, error)),
    }
}

#[must_use]
pub(super) fn unified_diff(edits: &[FileEdit]) -> String {
    edits
        .iter()
        .filter(|edit| edit.is_change())
        .map(|edit| {
            let name = edit.path.display();
            let (old_name, old_text) = match &edit.before {
                Some(text) => (format!("a{name}"), text.as_str()),
                None => ("/dev/null".to_owned(), ""),
            };
            let (new_name, new_text) = match &edit.after {
                Some(text) => (format!("b{name}"), text.as_str()),
                None => ("/dev/null".to_owned(), ""),
            };
            similar::TextDiff::from_lines(old_text, new_text)
                .unified_diff()
                .context_radius(3)
                .header(&old_name, &new_name)
                .to_string()
        })
        .collect()
}

pub(super) fn apply(
    edits: &[FileEdit],
    backups: &Backups,
    id: &str,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<ConnectOutcome, SetupError> {
    let diff = unified_diff(edits);
    let mut backup_path = String::new();
    if dry_run {
        return Ok(ConnectOutcome { diff, backup_path });
    }
    for edit in edits.iter().filter(|edit| edit.is_change()) {
        if edit.before.is_some() {
            let backup = backups.path_for(id, &edit.path, now);
            fs::create_dir_all(&backups.directory)
                .map_err(|error| io_error(&backups.directory, error))?;
            fs::copy(&edit.path, &backup).map_err(|error| io_error(&backup, error))?;
            if backup_path.is_empty() {
                backup_path = backup.display().to_string();
            }
        }
        match &edit.after {
            Some(text) => write_through(&edit.path, text)?,
            None => remove(&edit.path)?,
        }
    }
    Ok(ConnectOutcome { diff, backup_path })
}

fn remove(path: &Path) -> Result<(), SetupError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(path, error)),
    }
}

fn write_through(path: &Path, text: &str) -> Result<(), SetupError> {
    let target = if path.exists() {
        fs::canonicalize(path).map_err(|error| io_error(path, error))?
    } else {
        path.to_path_buf()
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    }
    let mut staged = target.clone().into_os_string();
    staged.push(".rh-staged");
    let staged = PathBuf::from(staged);
    fs::write(&staged, text).map_err(|error| io_error(&staged, error))?;
    if let Ok(metadata) = fs::metadata(&target) {
        fs::set_permissions(&staged, metadata.permissions())
            .map_err(|error| io_error(&staged, error))?;
    }
    fs::rename(&staged, &target).map_err(|error| io_error(&target, error))
}
