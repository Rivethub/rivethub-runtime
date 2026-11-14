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

    pub(crate) async fn served_url(&self) -> Result<String, Error> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DO $$ BEGIN CREATE ROLE {SERVICE} LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD '{SERVICE}'; \
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
        )))
        .execute(&self.pool)
        .await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER DATABASE \"{name}\" OWNER TO {SERVICE};
             DO $$ DECLARE item record; BEGIN
                FOR item IN SELECT n.nspname FROM pg_namespace n
                             WHERE n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema' LOOP
                    EXECUTE format('ALTER SCHEMA %I OWNER TO {SERVICE}', item.nspname);
                END LOOP;
                FOR item IN SELECT c.oid::regclass AS name FROM pg_class c
                              JOIN pg_namespace n ON n.oid = c.relnamespace
                             WHERE n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema'
                               AND c.relkind IN ('r', 'v', 'm', 'S', 'p')
                               AND NOT EXISTS (SELECT 1 FROM pg_depend d
                                                WHERE d.objid = c.oid AND d.deptype IN ('e', 'a', 'i')) LOOP
                    EXECUTE format('ALTER TABLE %s OWNER TO {SERVICE}', item.name);
                END LOOP;
                FOR item IN SELECT p.oid::regprocedure AS name FROM pg_proc p
                              JOIN pg_namespace n ON n.oid = p.pronamespace
                             WHERE n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema'
                               AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = p.oid AND d.deptype = 'e') LOOP
                    EXECUTE format('ALTER ROUTINE %s OWNER TO {SERVICE}', item.name);
                END LOOP;
                FOR item IN SELECT t.oid::regtype AS name FROM pg_type t
                              JOIN pg_namespace n ON n.oid = t.typnamespace
                             WHERE n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema'
                               AND t.typtype IN ('c', 'e', 'd') AND t.typrelid = 0
                               AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = t.oid AND d.deptype = 'e') LOOP
                    EXECUTE format('ALTER TYPE %s OWNER TO {SERVICE}', item.name);
                END LOOP;
             END $$",
            name = self.name
        )))
        .execute(&self.pool)
        .await?;
        let options = self
            .pool
            .connect_options()
            .as_ref()
            .clone()
            .username(SERVICE)
            .password(SERVICE);
        Ok(options.to_url_lossy().to_string())
    }
}

pub(crate) const SERVICE: &str = "rh_test_service";

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
