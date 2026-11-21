use std::borrow::Cow;
use std::collections::BTreeSet;
use std::time::Duration;

use sqlx::migrate::{Migrate, MigrateError, Migrator};
use sqlx::pool::PoolConnection;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgConnection, PgPool, Postgres};

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
    serving_login_ready(pool).await?;
    if !current_without_owner(pool).await? {
        postgres_apply(pool.acquire().await.map_err(Error::PostgresConnect)?).await?;
    }
    require_tracked(pool).await?;
    backfill(pool).await?;
    postgres_status_pool(pool).await
}

const SESSION_TRUST: i64 = 82;

/// A serving login cannot create the migration ledger, so a check with nothing
/// left to apply skips the migrator instead of failing on schema privileges.
async fn current_without_owner(pool: &PgPool) -> Result<bool, Error> {
    if postgres_status_pool(pool).await?.pending > 0 {
        return Ok(false);
    }
    let creates: bool =
        sqlx::query_scalar("SELECT has_schema_privilege(current_schema(), 'CREATE')")
            .fetch_one(pool)
            .await
            .map_err(Error::PostgresVersion)?;
    Ok(!creates)
}

async fn serving_login_ready(pool: &PgPool) -> Result<(), Error> {
    let applied = applied_versions(pool).await?;
    if applied.is_empty() || applied.contains(&SESSION_TRUST) {
        return Ok(());
    }
    let ready: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_roles r
                         WHERE r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolbypassrls
                           AND coalesce(pg_has_role(r.oid, to_regrole('rh_gateway'), 'MEMBER'), false)
                           AND has_database_privilege(r.oid, current_database(), 'CONNECT')
                           AND NOT pg_has_role(r.oid, (SELECT n.nspowner FROM pg_namespace n
                                                        WHERE n.oid = current_schema()::regnamespace), 'MEMBER'))",
    )
    .fetch_one(pool)
    .await
    .map_err(Error::PostgresVersion)?;
    if ready {
        Ok(())
    } else {
        Err(Error::NoServingLogin)
    }
}

const SUMMARY_SCHEMA: i64 = 76;
const SUMMARY_INDEXES: [(&str, &str, &str); 2] = [
    (
        "rh_summary_one_session",
        "DROP INDEX CONCURRENTLY rh_summary_one_session",
        "CREATE UNIQUE INDEX CONCURRENTLY rh_summary_one_session
         ON rh_summaries (conversation_id) WHERE kind = 'session'",
    ),
    (
        "rh_summary_sources_order",
        "DROP INDEX CONCURRENTLY rh_summary_sources_order",
        "CREATE INDEX CONCURRENTLY rh_summary_sources_order
         ON rh_summary_sources (summary_id, ordinal)",
    ),
];

pub async fn postgres_apply(mut conn: PoolConnection<Postgres>) -> Result<(), Error> {
    conn.close_on_drop();
    apply(&mut conn).await.map_err(Error::PostgresMigrate)
}

async fn apply(conn: &mut PgConnection) -> Result<(), MigrateError> {
    conn.lock().await?;
    let migrator = Migrator {
        migrations: Cow::Borrowed(POSTGRES.migrations.as_ref()),
        locking: false,
        ..Migrator::DEFAULT
    };
    migrator
        .run_direct(Some(SUMMARY_SCHEMA - 1), conn, false)
        .await?;
    let applied = conn.list_applied_migrations("_sqlx_migrations").await?;
    if !applied
        .iter()
        .any(|migration| migration.version == SUMMARY_SCHEMA)
    {
        prepare_summary_indexes(conn).await?;
        apply_summary_schema(&migrator, conn).await?;
    }
    migrator.run_direct(None, conn, false).await?;
    conn.unlock().await
}

async fn apply_summary_schema(
    migrator: &Migrator,
    conn: &mut PgConnection,
) -> Result<(), MigrateError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let result = migrator.run_direct(Some(SUMMARY_SCHEMA), conn, false).await;
        if !matches!(&result, Err(MigrateError::ExecuteMigration(sqlx::Error::Database(error), SUMMARY_SCHEMA))
            if error.code().as_deref() == Some("55P03"))
            || tokio::time::Instant::now() >= deadline
        {
            return result;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn prepare_summary_indexes(conn: &mut PgConnection) -> Result<(), MigrateError> {
    sqlx::query("SET max_parallel_maintenance_workers = 0")
        .execute(&mut *conn)
        .await?;
    for (name, drop, create) in SUMMARY_INDEXES {
        let valid: Option<bool> = sqlx::query_scalar(
            "SELECT indisvalid FROM pg_index WHERE indexrelid = to_regclass($1)",
        )
        .bind(name)
        .fetch_optional(&mut *conn)
        .await?;
        if valid == Some(true) {
            continue;
        }
        if valid == Some(false) {
            sqlx::query(drop).execute(&mut *conn).await?;
        }
        sqlx::query(create).execute(&mut *conn).await?;
    }
    Ok(())
}

const COUNT_BUDGET: i32 = 20_000;
const SAID_BATCH: i32 = 5_000;

async fn backfill(pool: &PgPool) -> Result<(), Error> {
    let present: bool = sqlx::query_scalar(
        "SELECT to_regprocedure('rh_backfill_message_counts(integer)') IS NOT NULL
                AND has_function_privilege('rh_backfill_message_counts(integer)', 'EXECUTE')",
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
