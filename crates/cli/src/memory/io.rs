use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use memory_sqlite::import::ImportTable;

use super::target::Origin;
use super::target::Source;
use crate::Error;
use crate::Report;

pub(super) struct Header {
    source: Option<Source>,
    host: Option<String>,
}

pub(super) struct Loaded {
    pub(super) text: String,
    spool: Option<tempfile::NamedTempFile>,
    original: PathBuf,
}

impl Loaded {
    pub(super) fn dump_path(&self) -> PathBuf {
        match &self.spool {
            Some(file) => file.path().to_path_buf(),
            None => self.original.clone(),
        }
    }
}

#[derive(serde::Serialize)]
struct ImportSummary {
    source: Source,
    host: String,
    rows: BTreeMap<String, u64>,
    dry_run: bool,
}

pub(super) async fn read_dump(path: &Path, keep_file: bool) -> Result<Loaded, Error> {
    let bytes = tokio::fs::read(path).await?;
    let gzipped = is_gzip(&bytes);
    let plain = if gzipped {
        tokio::task::spawn_blocking(move || gunzip(&bytes)).await??
    } else {
        bytes
    };
    let text = String::from_utf8(plain).map_err(|_| Error::DumpEncoding)?;
    let spool = if gzipped && keep_file {
        let encoded = text.clone().into_bytes();
        Some(tokio::task::spawn_blocking(move || spool_file(&encoded)).await??)
    } else {
        None
    };
    Ok(Loaded {
        text,
        spool,
        original: path.to_path_buf(),
    })
}

pub(super) fn gzip(plain: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(plain)?;
    encoder.finish()
}

pub(super) fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let parent = parent_dir(path)?;
    if !parent.is_dir() {
        return Err(Error::OutputDirectory);
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => return Err(Error::OutputSymlink),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(Error::OutputName);
    };
    let temporary = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    if let Err(error) = write_temp(&temporary, bytes) {
        remove_temp(&temporary)?;
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        remove_temp(&temporary)?;
        return Err(error.into());
    }
    Ok(())
}

pub(super) fn preamble(text: &str) -> Result<Header, Error> {
    let mut source = None;
    let mut host = None;
    for line in text.lines() {
        if !(line.is_empty() || line.starts_with("--")) {
            break;
        }
        if let Some(value) = line.strip_prefix("-- source ") {
            let value = value.trim();
            source = Some(
                value
                    .parse::<Source>()
                    .map_err(|_| Error::UnknownDumpSource(value.to_owned()))?,
            );
        } else if let Some(value) = line.strip_prefix("-- host ") {
            let value = value.trim();
            if value.is_empty() {
                return Err(Error::DumpHost);
            }
            host = Some(value.to_owned());
        }
    }
    Ok(Header { source, host })
}

pub(super) fn finish_origin(header: Header, fallback: Origin) -> Origin {
    Origin {
        source: match header.source {
            Some(source) => source,
            None => fallback.source,
        },
        host: match header.host {
            Some(host) => host,
            None => fallback.host,
        },
    }
}

pub(super) fn summary_report(
    origin: Origin,
    rows: BTreeMap<String, u64>,
    dry_run: bool,
) -> Result<Report, Error> {
    let body = ImportSummary {
        source: origin.source,
        host: origin.host,
        rows,
        dry_run,
    };
    Ok(Report::success(format!(
        "{}\n",
        serde_json::to_string(&body)?
    )))
}

pub(super) fn sqlite_rows(rows: BTreeMap<ImportTable, u64>) -> BTreeMap<String, u64> {
    rows.into_iter()
        .map(|(table, count)| (table.to_string(), count))
        .collect()
}

fn is_gzip(bytes: &[u8]) -> bool {
    bytes.first().copied() == Some(0x1f) && bytes.get(1).copied() == Some(0x8b)
}

fn gunzip(bytes: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Read;
    let mut decoder = flate2::read::GzDecoder::new(bytes);
    let mut plain = Vec::new();
    decoder.read_to_end(&mut plain)?;
    Ok(plain)
}

fn spool_file(bytes: &[u8]) -> Result<tempfile::NamedTempFile, std::io::Error> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    Ok(file)
}

fn parent_dir(path: &Path) -> Result<&Path, Error> {
    match path.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Ok(Path::new(".")),
        Some(parent) => Ok(parent),
        None => Err(Error::OutputDirectory),
    }
}

fn write_temp(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn remove_temp(path: &Path) -> Result<(), Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
