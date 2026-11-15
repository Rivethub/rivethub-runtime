use std::collections::BTreeSet;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::Status;
use crate::error::Error;
use crate::error::relation_missing;

pub static POSTGRES: sqlx::migrate::Migrator = sqlx::migrate!();

pub async fn postgres(url: &str) -> Result<Status, Error> {
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SELECT set_config('rh.maintenance', 'on', false)")
                    .execute(conn)
                    .await
                    .map(drop)
            })
        })
        .connect(url)
        .await
        .map_err(Error::PostgresConnect)?;
    let status = postgres_pool(&pool).await;
    pool.close().await;
    status
}

pub async fn postgres_pool(pool: &PgPool) -> Result<Status, Error> {
    POSTGRES.run(pool).await.map_err(Error::PostgresMigrate)?;
    require_tracked(pool).await?;
    backfill(pool).await?;
    postgres_status_pool(pool).await
}

const COUNT_BUDGET: i32 = 20_000;
const SAID_BATCH: i32 = 5_000;

async fn backfill(pool: &PgPool) -> Result<(), Error> {
    let present: bool = sqlx::query_scalar(
        "SELECT to_regprocedure('rh_backfill_message_counts(integer)') IS NOT NULL",
    )
    .fetch_one(pool)
    .await
    .map_err(Error::PostgresBackfill)?;
    if !present {
        return Ok(());
    }
    loop {
        let left: i64 = sqlx::query_scalar("SELECT rh_backfill_message_counts($1)")
            .bind(COUNT_BUDGET)
            .fetch_one(pool)
            .await
            .map_err(Error::PostgresBackfill)?;
        if left == 0 {
            break;
        }
    }
    let mut after: Option<sqlx::types::Uuid> = None;
    loop {
        let last: Option<sqlx::types::Uuid> =
            sqlx::query_scalar("SELECT rh_backfill_chunk_said($1, $2)")
                .bind(after)
                .bind(SAID_BATCH)
                .fetch_one(pool)
                .await
                .map_err(Error::PostgresBackfill)?;
        if last.is_none() {
            break;
        }
        after = last;
    }
    Ok(())
}

pub async fn postgres_status(url: &str) -> Result<Status, Error> {
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await
        .map_err(Error::PostgresConnect)?;
    let status = postgres_status_pool(&pool).await;
    pool.close().await;
    status
}

pub async fn postgres_status_pool(pool: &PgPool) -> Result<Status, Error> {
    let required = required()?;
    let current = newest_applied(pool).await?;
    let applied = applied_versions(pool).await?;
    Ok(Status {
        current,
        required,
        pending: POSTGRES
            .migrations
            .iter()
            .filter(|migration| {
                migration.migration_type.is_up_migration() && !applied.contains(&migration.version)
            })
            .count(),
    })
}

pub fn required() -> Result<i64, Error> {
    POSTGRES
        .migrations
        .iter()
        .map(|migration| migration.version)
        .max()
        .ok_or(Error::PostgresEmpty)
}

async fn require_tracked(pool: &PgPool) -> Result<(), Error> {
    let present = tracked_names(pool).await?;
    let missing: Vec<String> = expected_migration_files()
        .into_iter()
        .filter(|name| !present.contains(name))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Error::TrackingMissing {
            missing: missing.join(", "),
        })
    }
}

fn expected_migration_files() -> BTreeSet<String> {
    POSTGRES
        .migrations
        .iter()
        .filter(|migration| migration.migration_type.is_up_migration())
        .map(migration_file_name)
        .collect()
}

fn migration_file_name(migration: &sqlx::migrate::Migration) -> String {
    let description = migration.description.replace(' ', "_");
    format!(
        "{:04}_{description}{}",
        migration.version,
        migration.migration_type.suffix()
    )
}

async fn tracked_names(pool: &PgPool) -> Result<BTreeSet<String>, Error> {
    match sqlx::query_scalar!("SELECT name FROM _rivetos_migrations")
        .fetch_all(pool)
        .await
    {
        Ok(names) => Ok(names.into_iter().collect()),
        Err(source) if relation_missing(&source) => Ok(BTreeSet::new()),
        Err(source) => Err(Error::PostgresVersion(source)),
    }
}

async fn applied_versions(pool: &PgPool) -> Result<BTreeSet<i64>, Error> {
    match sqlx::query_scalar!("select version from _sqlx_migrations where success")
        .fetch_all(pool)
        .await
    {
        Ok(versions) => Ok(versions.into_iter().collect()),
        Err(source) if relation_missing(&source) => Ok(BTreeSet::new()),
        Err(source) => Err(Error::PostgresVersion(source)),
    }
}

async fn newest_applied(pool: &PgPool) -> Result<i64, Error> {
    match sqlx::query!(
        "select coalesce(max(version), 0) as \"applied!\" from _sqlx_migrations where success"
    )
    .fetch_one(pool)
    .await
    {
        Ok(row) => Ok(row.applied),
        Err(source) if relation_missing(&source) => Ok(0),
        Err(source) => Err(Error::PostgresVersion(source)),
    }
}
