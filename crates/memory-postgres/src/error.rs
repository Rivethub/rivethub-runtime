use kernel::memory::MemoryStoreError;
use kernel::queue::JobQueueError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("wiki page escapes its directory")]
    WikiContainment,
    #[error("wiki file access failed: {0}")]
    WikiIo(#[source] std::io::Error),
    #[error("wiki scope: {0}")]
    WikiScope(#[source] kernel::wiki::builder::WikiStoreError),
    #[error("capture channel is required")]
    MissingChannel,
    #[error("time windows must be resolved by the caller with its time zone")]
    UnresolvedWindow,
    #[error("invalid memory id: {0}")]
    InvalidId(#[from] uuid::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Integer(#[from] std::num::TryFromIntError),
    #[error("invalid stored {field}: {source}")]
    StoredValue {
        field: &'static str,
        #[source]
        source: kernel::BoxError,
    },
    #[error("project lookup failed")]
    ProjectIo(#[from] std::io::Error),
    #[error("project lookup returned invalid UTF-8")]
    ProjectEncoding(#[from] std::string::FromUtf8Error),
    #[error("project lookup task failed")]
    ProjectTask(#[from] tokio::task::JoinError),
    #[error("connection to the memory database failed")]
    Connection(#[source] sqlx::Error),
    #[error(
        "the memory schema is behind: the database reports version {found}, this build requires {required} — run rh db migrate"
    )]
    SchemaBehind { found: i64, required: i64 },
    #[error("this database has no RivetHub migration history — run rh db migrate once to adopt it")]
    NotAdopted,
    #[error("the graphile_worker schema is absent on this database")]
    QueueAbsent,
    #[error("Postgres dump line {line}: {detail}")]
    DumpFormat { line: usize, detail: String },
    #[error("Postgres import refused: target table {table} is not empty")]
    DumpOccupied { table: &'static str },
    #[error("Postgres dump is incomplete: expected COPY blocks and the pg_dump completion footer")]
    DumpTruncated,
    #[error("Postgres dump repeats table {table}")]
    DumpDuplicate { table: &'static str },
    #[error("memory dump cancelled")]
    DumpCancelled,
    #[error("{0}")]
    DumpMessage(String),
    #[error("merge: {0}")]
    Merge(String),
    #[error("{operation} failed: {message}")]
    Operation {
        operation: &'static str,
        message: String,
        #[source]
        source: sqlx::Error,
    },
}

impl Error {
    pub(crate) fn stored(
        field: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::StoredValue {
            field,
            source: Box::new(source),
        }
    }

    pub(crate) fn operation(operation: &'static str, source: sqlx::Error) -> Self {
        if crate::loss::unreachable(&source) {
            return Self::Connection(source);
        }
        Self::Operation {
            operation,
            message: source.to_string(),
            source,
        }
    }

    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::Connection(sqlx::Error::Protocol(message.into()))
    }

    pub(crate) fn relation_missing(source: &sqlx::Error) -> bool {
        source
            .as_database_error()
            .is_some_and(|error| error.code().as_deref() == Some("42P01"))
    }
}

impl From<Error> for MemoryStoreError {
    fn from(value: Error) -> Self {
        match value {
            Error::Connection(source) => Self::Unavailable(Box::new(source)),
            Error::SchemaBehind { found, required } => {
                Self::Unavailable(Box::new(Error::SchemaBehind { found, required }))
            }
            Error::NotAdopted => Self::Unavailable(Box::new(Error::NotAdopted)),
            Error::QueueAbsent => Self::Unavailable(Box::new(Error::QueueAbsent)),
            Error::MissingChannel
            | Error::WikiContainment
            | Error::UnresolvedWindow
            | Error::InvalidId(_)
            | Error::Json(_)
            | Error::DumpFormat { .. }
            | Error::DumpOccupied { .. }
            | Error::DumpTruncated
            | Error::DumpDuplicate { .. }
            | Error::DumpCancelled
            | Error::DumpMessage(_)
            | Error::Merge(_) => Self::InvalidRequest {
                message: value.to_string(),
            },
            Error::Integer(_)
            | Error::WikiIo(_)
            | Error::WikiScope(_)
            | Error::ProjectIo(_)
            | Error::ProjectEncoding(_)
            | Error::ProjectTask(_) => Self::Unavailable(Box::new(value)),
            error @ Error::StoredValue { .. } => Self::Query {
                message: error.to_string(),
                source: Box::new(error),
            },
            Error::Operation {
                message, source, ..
            } if unstorable_text(&source) => Self::InvalidRequest { message },
            error @ Error::Operation { .. } if cancelled(&error) => {
                Self::Unavailable(Box::new(error))
            }
            Error::Operation {
                message, source, ..
            } => Self::Query {
                message,
                source: Box::new(source),
            },
        }
    }
}

fn cancelled(error: &Error) -> bool {
    matches!(error, Error::Operation { source, .. } if source
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "57014"))
}

fn unstorable_text(source: &sqlx::Error) -> bool {
    source
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| matches!(code.as_ref(), "22021" | "22P05"))
}

impl From<Error> for JobQueueError {
    fn from(value: Error) -> Self {
        match value {
            Error::Connection(source) => Self::Unavailable(Box::new(source)),
            Error::SchemaBehind { found, required } => {
                Self::Unavailable(Box::new(Error::SchemaBehind { found, required }))
            }
            Error::NotAdopted => Self::Unavailable(Box::new(Error::NotAdopted)),
            Error::QueueAbsent => Self::Unavailable(Box::new(Error::QueueAbsent)),
            Error::MissingChannel
            | Error::WikiContainment
            | Error::WikiIo(_)
            | Error::WikiScope(_)
            | Error::UnresolvedWindow
            | Error::InvalidId(_)
            | Error::Json(_)
            | Error::Integer(_)
            | Error::ProjectIo(_)
            | Error::ProjectEncoding(_)
            | Error::ProjectTask(_)
            | Error::DumpFormat { .. }
            | Error::DumpOccupied { .. }
            | Error::DumpTruncated
            | Error::DumpDuplicate { .. }
            | Error::DumpCancelled
            | Error::DumpMessage(_)
            | Error::Merge(_) => Self::Unavailable(Box::new(value)),
            error @ Error::StoredValue { .. } => Self::Claim {
                message: error.to_string(),
                source: Box::new(error),
            },
            Error::Operation {
                message, source, ..
            } => Self::Claim {
                message,
                source: Box::new(source),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Error;
    use kernel::memory::MemoryStoreError;
    use kernel::queue::JobQueueError;

    #[test]
    fn schema_refusals_box_the_typed_error() {
        assert_store(Error::SchemaBehind {
            found: 3,
            required: 21,
        });
        assert_queue(Error::SchemaBehind {
            found: 3,
            required: 21,
        });
        assert_store(Error::NotAdopted);
        assert_queue(Error::NotAdopted);
    }

    fn assert_store(error: Error) {
        let text = error.to_string();
        let MemoryStoreError::Unavailable(source) = MemoryStoreError::from(error) else {
            panic!("expected Unavailable");
        };
        let typed = source.downcast::<Error>().expect("boxed typed error");
        assert_eq!(typed.to_string(), text);
    }

    #[test]
    fn pool_timeout_is_unavailable() {
        let error = Error::operation("probe", sqlx::Error::PoolTimedOut);
        assert!(matches!(
            MemoryStoreError::from(error),
            MemoryStoreError::Unavailable(_)
        ));
        let error = Error::operation("probe", sqlx::Error::PoolTimedOut);
        assert!(matches!(
            JobQueueError::from(error),
            JobQueueError::Unavailable(_)
        ));
    }

    #[test]
    fn ssl_request_garbage_is_unavailable() {
        let error = Error::operation(
            "claim",
            sqlx::Error::Protocol(
                "unexpected response from SSLRequest: 0x00 (sqlx_postgres::connection::tls:95)"
                    .into(),
            ),
        );
        assert!(matches!(
            JobQueueError::from(error),
            JobQueueError::Unavailable(_)
        ));
    }

    fn assert_queue(error: Error) {
        let text = error.to_string();
        let JobQueueError::Unavailable(source) = JobQueueError::from(error) else {
            panic!("expected Unavailable");
        };
        let typed = source.downcast::<Error>().expect("boxed typed error");
        assert_eq!(typed.to_string(), text);
    }
}
