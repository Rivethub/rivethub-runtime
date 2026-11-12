use std::ops::Deref;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

use sqlx::ConnectOptions;
use sqlx::PgPool;
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPoolOptions;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("DATABASE_URL is not usable: {0}")]
    Url(String),
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

static NEXT: AtomicU32 = AtomicU32::new(0);

#[derive(Debug)]
pub(crate) struct TestDb {
    pool: PgPool,
    name: String,
    admin: PgConnectOptions,
}

impl TestDb {
    pub(crate) async fn bare() -> Result<Self, Error> {
        Self::create(false).await
    }

    pub(crate) async fn migrated() -> Result<Self, Error> {
        Self::create(true).await
    }

    async fn create(migrate: bool) -> Result<Self, Error> {
        let url = config::Environment::from_process()
            .text("DATABASE_URL")
            .ok_or_else(|| Error::Url("not set".to_owned()))?
            .to_owned();
        let admin: PgConnectOptions = url
            .parse()
            .map_err(|error: sqlx::Error| Error::Url(error.to_string()))?;
        let name = format!(
            "rh_t_{}_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let mut conn = admin.connect().await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
            .execute(&mut conn)
            .await?;
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(admin.clone().database(&name))
            .await?;
        if migrate {
            migrate::POSTGRES.run(&pool).await?;
        }
        Ok(Self { pool, name, admin })
    }

    pub(crate) fn pool(&self) -> PgPool {
        self.pool.clone()
    }
}

impl Deref for TestDb {
    type Target = PgPool;

    fn deref(&self) -> &PgPool {
        &self.pool
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let name = std::mem::take(&mut self.name);
        let admin = self.admin.clone();
        let cleanup = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async {
                if let Ok(mut conn) = admin.connect().await {
                    drop(
                        sqlx::query(sqlx::AssertSqlSafe(format!(
                            "DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"
                        )))
                        .execute(&mut conn)
                        .await,
                    );
                }
            });
        });
        drop(cleanup.join());
    }
}
