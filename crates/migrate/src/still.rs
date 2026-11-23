use std::path::Path;
use std::path::PathBuf;

use rusqlite::Connection;

use crate::error::Error;

const ATTEMPTS: u8 = 3;

#[derive(Debug)]
pub(crate) struct Still {
    reader: Connection,
}

impl Still {
    pub(crate) fn release(self) {
        drop(self.reader);
    }
}

pub(crate) fn hold(path: &Path) -> Result<Still, Error> {
    let reader = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|source| Error::SqliteOpen {
            path: path.to_path_buf(),
            source,
        })?;
    let failed = |source| Error::BackupPrepare {
        path: path.to_path_buf(),
        source,
    };
    for _ in 0..ATTEMPTS {
        let blocked: i64 = reader
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .map_err(failed)?;
        if blocked != 0 {
            break;
        }
        reader.execute_batch("BEGIN").map_err(failed)?;
        reader
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(failed)?;
        if log_is_empty(path)? {
            return Ok(Still { reader });
        }
        reader.execute_batch("ROLLBACK").map_err(failed)?;
    }
    Err(Error::InUse {
        path: path.to_path_buf(),
    })
}

fn log_is_empty(path: &Path) -> Result<bool, Error> {
    let mut name = path.as_os_str().to_os_string();
    name.push("-wal");
    let log = PathBuf::from(name);
    match std::fs::metadata(&log) {
        Ok(found) => Ok(found.len() == 0),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(source) => Err(Error::Inspect { path: log, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::hold;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn a_held_file_is_not_changed_by_another_connection() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("memory.sqlite");
        let other = rusqlite::Connection::open(&path)?;
        other.execute_batch(
            "PRAGMA journal_mode = wal; PRAGMA wal_autocheckpoint = 0;
             CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('before');",
        )?;
        other.busy_timeout(std::time::Duration::ZERO)?;
        let still = hold(&path)?;
        let held = std::fs::read(&path)?;
        other.execute_batch("INSERT INTO notes VALUES ('during')")?;
        let blocked: i64 = other.query_row("PRAGMA wal_checkpoint(FULL)", [], |row| row.get(0))?;
        assert_eq!((blocked, std::fs::read(&path)? == held), (1, true));
        still.release();
        let blocked: i64 = other.query_row("PRAGMA wal_checkpoint(FULL)", [], |row| row.get(0))?;
        assert_eq!((blocked, std::fs::read(&path)? == held), (0, false));
        Ok(())
    }
}
