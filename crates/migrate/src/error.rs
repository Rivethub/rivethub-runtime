use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("inspecting memory file {path} failed: {source}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("connecting to postgres failed")]
    PostgresConnect(#[source] sqlx::Error),
    #[error("applying the postgres migrations failed")]
    PostgresMigrate(#[source] sqlx::migrate::MigrateError),
    #[error("reading the applied postgres schema version failed")]
    PostgresVersion(#[source] sqlx::Error),
    #[error("backfilling the postgres memory counts failed")]
    PostgresBackfill(#[source] sqlx::Error),
    #[error("missing _rivetos_migrations rows: {missing}")]
    TrackingMissing { missing: String },
    #[error("the postgres migration set is empty")]
    PostgresEmpty,
    #[error("the memory file at {path} could not be opened")]
    SqliteOpen {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("a memory schema pragma failed")]
    SqlitePragma(#[source] rusqlite::Error),
    #[error("the memory schema could not be created")]
    SqliteCreate(#[source] rusqlite::Error),
    #[error("the memory schema upgrade to version {version} failed: {source}")]
    SqliteUpgrade {
        version: i64,
        #[source]
        source: rusqlite::Error,
    },
    #[error("the memory schema version {found} is not supported by this build")]
    Unsupported { found: i64 },
    #[error("copying {path} to {backup} failed")]
    Backup {
        path: PathBuf,
        backup: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("a backup already exists at {path} — refusing to replace it")]
    BackupExists { path: PathBuf },
    #[error("a legacy summary kind has no depth mapping")]
    UnmappedSummaryKind,
}

pub(crate) fn relation_missing(source: &sqlx::Error) -> bool {
    source
        .as_database_error()
        .is_some_and(|error| error.code().as_deref() == Some("42P01"))
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn sqlite_upgrade_display_includes_the_sqlite_error() {
        let connection = rusqlite::Connection::open_in_memory().expect("memory database");
        let source = connection.execute("NOT SQL", []).expect_err("bad sql");
        let rendered = source.to_string();
        let error = Error::SqliteUpgrade {
            version: 18,
            source,
        };
        assert_eq!(
            error.to_string(),
            format!("the memory schema upgrade to version 18 failed: {rendered}")
        );
        assert!(
            error.to_string().contains("version 18"),
            "the version stays visible"
        );
        assert!(
            std::error::Error::source(&error).is_some(),
            "callers can still walk the source chain"
        );
    }
}
