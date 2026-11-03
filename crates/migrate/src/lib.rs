pub mod error;
mod foreign;
mod ids;
mod postgres;
mod schema;
mod upgrade;
mod version;

pub use error::Error;
pub use foreign::foreign_line;
pub use foreign::sqlite_foreign;
pub use postgres::POSTGRES;
pub use postgres::postgres;
pub use postgres::postgres_pool;
pub use postgres::postgres_status;
pub use postgres::postgres_status_pool;
pub use postgres::required;
pub use schema::sqlite;
pub use schema::sqlite_at;
pub use schema::sqlite_status;
pub use version::SchemaVersion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub current: i64,
    pub required: i64,
    pub pending: usize,
}
