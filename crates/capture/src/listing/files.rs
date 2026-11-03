use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::hash::Hash;
use std::io;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use kernel::transcript::SessionRenameError;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::TranscriptWindow;
use serde::Deserialize;

use super::field::Field;
use super::field::object_line;
use super::field::text_of;
use super::time::FileTimes;
use crate::store_path::Child;
use crate::store_path::Root;
use crate::store_path::is_not_found;

pub(crate) const TAIL_WINDOW: u64 = 8 * 1024 * 1024;
const LINE_CAP: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) kind: Child,
}

impl Entry {
    pub(crate) fn is_dir(&self) -> bool {
        self.kind == Child::Directory
    }

    pub(crate) fn is_file(&self) -> bool {
        self.kind == Child::File
    }

    pub(crate) fn hidden(&self) -> bool {
        self.name.starts_with('.')
    }
}

pub(crate) fn open_store(path: &Path) -> Result<Option<Root>, TranscriptReaderError> {
    match Root::open(path) {
        Ok(root) => Ok(Some(root)),
        Err(error) if is_not_found(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn entries(root: &Root, dir: &Path) -> Vec<Entry> {
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(error) => {
            tracing::debug!(%error, dir = %dir.display(), "store directory is unreadable and is skipped");
            return Vec::new();
        }
    };
    let mut found = Vec::new();
    for item in listing {
        let path = match item {
            Ok(item) => item.path(),
            Err(error) => {
                tracing::debug!(%error, dir = %dir.display(), "store directory entry is unreadable and is skipped");
                continue;
            }
        };
        let Some(name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        match root.classify(&path) {
            Ok(kind) => found.push(Entry { name, path, kind }),
            Err(error) => {
                tracing::debug!(%error, "store entry is refused and is skipped");
            }
        }
    }
    found.sort_by(|left, right| left.name.cmp(&right.name));
    found
}

pub(crate) fn directories(root: &Root, dir: &Path) -> Vec<Entry> {
    entries(root, dir)
        .into_iter()
        .filter(Entry::is_dir)
        .collect()
}

pub(crate) fn file_times(path: &Path) -> Option<FileTimes> {
    match fs::metadata(path) {
        Ok(metadata) => Some(FileTimes::of(&metadata)),
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "store file cannot be stat'ed and is skipped");
            None
        }
    }
}

pub(crate) fn regular_file(root: &Root, path: &Path) -> bool {
    match root.classify(path) {
        Ok(kind) => kind == Child::File,
        Err(error) => {
            tracing::debug!(%error, "store file is refused and is skipped");
            false
        }
    }
}

pub(crate) fn unreadable(path: &Path, error: &io::Error) {
    tracing::debug!(%error, path = %path.display(), "store file is unreadable and contributes no metadata");
}

pub(crate) fn whole_file(path: &Path) -> Option<String> {
    match fs::read(path) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(error) => {
            unreadable(path, &error);
            None
        }
    }
}

pub(crate) fn head(path: &Path, limit: u64) -> Option<String> {
    let mut bytes = Vec::new();
    let read = File::open(path).and_then(|file| file.take(limit).read_to_end(&mut bytes));
    match read {
        Ok(_) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(error) => {
            unreadable(path, &error);
            None
        }
    }
}

pub(crate) fn tail_window(path: &Path) -> Option<String> {
    tail(path, TAIL_WINDOW)
}

pub(crate) fn tail(path: &Path, limit: u64) -> Option<String> {
    let window = File::open(path)
        .and_then(|file| Ok(file.metadata()?.len()))
        .and_then(|length| {
            read_span(
                path,
                Span {
                    from: length.saturating_sub(limit),
                    to: length,
                    at_line: false,
                },
            )
        });
    match window {
        Ok(window) => Some(window.text),
        Err(error) => {
            unreadable(path, &error);
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) from: u64,
    pub(crate) to: u64,
    pub(crate) at_line: bool,
}

impl Span {
    pub(crate) fn of(path: &Path, wanted: TranscriptWindow) -> io::Result<Self> {
        let length = fs::metadata(path)?.len();
        let (from, to, at_line) = match wanted {
            TranscriptWindow::Latest => (length.saturating_sub(TAIL_WINDOW), length, false),
            TranscriptWindow::From(start) => (start.min(length), length, true),
            TranscriptWindow::Before(end) => {
                let end = end.min(length);
                (end.saturating_sub(TAIL_WINDOW), end, false)
            }
        };
        Ok(Self { from, to, at_line })
    }

    pub(crate) fn widened(self) -> Self {
        Self {
            from: self.from.saturating_sub(TAIL_WINDOW),
            ..self
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) text: String,
    pub(crate) start: u64,
}

pub(crate) fn read_span(path: &Path, span: Span) -> io::Result<Window> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(span.from))?;
    let mut bytes = Vec::new();
    file.take(span.to.saturating_sub(span.from))
        .read_to_end(&mut bytes)?;
    let skip = if span.from == 0 || span.at_line {
        0
    } else {
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |newline| newline + 1)
    };
    Ok(Window {
        text: String::from_utf8_lossy(bytes.get(skip..).unwrap_or_default()).into_owned(),
        start: span.from + skip as u64,
    })
}

pub(crate) fn append_line(path: &Path, line: &str) -> io::Result<()> {
    let mut file = File::options().read(true).append(true).open(path)?;
    let length = file.metadata()?.len();
    let mut record = String::with_capacity(line.len() + 2);
    if length > 0 {
        let mut last = [0_u8; 1];
        file.seek(SeekFrom::Start(length - 1))?;
        file.read_exact(&mut last)?;
        if &last != b"\n" {
            record.push('\n');
        }
    }
    record.push_str(line);
    record.push('\n');
    file.write_all(record.as_bytes())
}

pub(crate) fn complete_lines(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |newline| newline + 1);
    bytes.get(..end).unwrap_or_default()
}

#[derive(Debug, Deserialize)]
struct CwdLine {
    cwd: Option<Field>,
}

pub(crate) fn first_cwd(path: &Path) -> Result<Option<String>, TranscriptReaderError> {
    let file = File::open(path).map_err(|error| read_failure(path, error))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut reader)
            .take(LINE_CAP + 1)
            .read_until(b'\n', &mut line)
            .map_err(|error| read_failure(path, error))?;
        if read == 0 {
            return Ok(None);
        }
        if line.last() != Some(&b'\n') && line.len() as u64 > LINE_CAP {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&line);
        let cwd = object_line::<CwdLine>(&text)
            .and_then(|parsed| text_of(parsed.cwd.as_ref()))
            .map(|cwd| cwd.trim().to_owned())
            .filter(|cwd| !cwd.is_empty());
        if cwd.is_some() {
            return Ok(cwd);
        }
    }
}

pub(crate) fn read_failure(path: &Path, error: io::Error) -> TranscriptReaderError {
    TranscriptReaderError::Read {
        message: format!("session store file {}: {error}", path.display()),
        source: Box::new(error),
    }
}

pub(crate) fn write_failure(path: &Path, error: io::Error) -> SessionRenameError {
    SessionRenameError::Write {
        message: format!("session store file {}: {error}", path.display()),
        source: Box::new(error),
    }
}

pub(crate) fn newest_per_key<T, K: Hash + Eq>(
    found: Vec<T>,
    key: impl Fn(&T) -> K,
    mtime: impl Fn(&T) -> i64,
) -> Vec<T> {
    let mut index: HashMap<K, usize> = HashMap::new();
    let mut kept: Vec<T> = Vec::new();
    for item in found {
        let item_key = key(&item);
        if let Some(current) = index
            .get(&item_key)
            .and_then(|position| kept.get_mut(*position))
        {
            if mtime(&item) >= mtime(current) {
                *current = item;
            }
        } else {
            index.insert(item_key, kept.len());
            kept.push(item);
        }
    }
    kept
}
