pub(crate) fn unreachable(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::PoolClosed
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::WorkerCrashed => true,
        sqlx::Error::Database(database) => connection_code(database.code().as_deref()),
        sqlx::Error::Protocol(message) => broken_handshake(message),
        sqlx::Error::Configuration(_)
        | sqlx::Error::InvalidArgument(_)
        | sqlx::Error::RowNotFound
        | sqlx::Error::TypeNotFound { .. }
        | sqlx::Error::ColumnIndexOutOfBounds { .. }
        | sqlx::Error::ColumnNotFound(_)
        | sqlx::Error::ColumnDecode { .. }
        | sqlx::Error::Encode(_)
        | sqlx::Error::Decode(_)
        | sqlx::Error::AnyDriverError(_)
        | sqlx::Error::Migrate(_)
        | sqlx::Error::InvalidSavePointStatement
        | sqlx::Error::BeginFailed
        | _ => false,
    }
}

fn connection_code(code: Option<&str>) -> bool {
    match code {
        Some("53300" | "57P01" | "57P02" | "57P03" | "57P04" | "57P05") => true,
        Some(code) => code.starts_with("08"),
        None => false,
    }
}

fn broken_handshake(message: &str) -> bool {
    message.contains("SSLRequest")
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::unreachable;

    #[derive(Debug)]
    struct Coded {
        code: &'static str,
    }

    impl std::fmt::Display for Coded {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(self.code)
        }
    }

    impl std::error::Error for Coded {}

    impl sqlx::error::DatabaseError for Coded {
        fn message(&self) -> &str {
            self.code
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.code))
        }

        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    fn database(code: &'static str) -> sqlx::Error {
        sqlx::Error::Database(Box::new(Coded { code }))
    }

    #[test]
    fn transport_and_shutdown_codes_are_connection_loss() {
        assert!(unreachable(&sqlx::Error::PoolTimedOut));
        assert!(unreachable(&sqlx::Error::PoolClosed));
        assert!(unreachable(&sqlx::Error::WorkerCrashed));
        assert!(unreachable(&sqlx::Error::Io(std::io::Error::other(
            "reset"
        ))));
        assert!(unreachable(&database("08006")));
        assert!(unreachable(&database("57P01")));
        assert!(unreachable(&database("53300")));
        assert!(unreachable(&sqlx::Error::Protocol(
            "unexpected response from SSLRequest: 0x00 (sqlx_postgres::connection::tls:95)".into(),
        )));
    }

    #[test]
    fn only_a_cancelled_statement_counts_as_timed_out() {
        use crate::error::Error;
        assert!(Error::operation("search", database("57014")).timed_out());
        assert!(!Error::operation("search", database("42P01")).timed_out());
        assert!(!Error::operation("search", database("08006")).timed_out());
        assert!(!Error::operation("search", sqlx::Error::RowNotFound).timed_out());
    }

    #[test]
    fn statement_timeout_stays_a_query_failure() {
        assert!(!unreachable(&database("57014")));
        assert!(!unreachable(&sqlx::Error::RowNotFound));
        assert!(!unreachable(&sqlx::Error::Protocol(
            "unexpected message type".into(),
        )));
    }
}
