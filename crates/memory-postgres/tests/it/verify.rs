use memory_postgres::Error;
use memory_postgres::PostgresMemory;
use memory_postgres::SCHEMA_VERSION;
use sqlx::ConnectOptions as _;
use sqlx::PgPool;
use tracing::instrument::WithSubscriber as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn url(pool: &PgPool) -> String {
    pool.connect_options().to_url_lossy().to_string()
}

#[sqlx::test(migrations = false)]
async fn a_fresh_database_is_not_adopted_and_left_untouched(pool: PgPool) -> TestResult {
    match PostgresMemory::connect(&url(&pool)).await {
        Err(error @ Error::NotAdopted) => assert_eq!(
            error.to_string(),
            "this database has no RivetHub migration history — run rh db migrate once to adopt it"
        ),
        other => panic!("expected NotAdopted, got {other:?}"),
    }
    let tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(tables, 0, "the runtime must not create anything");
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn an_empty_migration_table_is_schema_behind(pool: PgPool) -> TestResult {
    sqlx::query(
        "CREATE TABLE _sqlx_migrations (
            version BIGINT PRIMARY KEY,
            description TEXT NOT NULL,
            installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),
            success BOOLEAN NOT NULL,
            checksum BYTEA NOT NULL,
            execution_time BIGINT NOT NULL
        )",
    )
    .execute(&pool)
    .await?;
    match PostgresMemory::connect(&url(&pool)).await {
        Err(Error::SchemaBehind { found, required }) => {
            assert_eq!(found, 0);
            assert_eq!(required, SCHEMA_VERSION);
        }
        other => panic!("expected SchemaBehind, got {other:?}"),
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_migrated_database_connects(pool: PgPool) -> TestResult {
    PostgresMemory::connect(&url(&pool)).await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_newer_schema_is_accepted_with_one_warning(pool: PgPool) -> TestResult {
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, execution_time, checksum)
         VALUES (9999, 'fake-newer', true, 0, '\\x'::bytea)",
    )
    .execute(&pool)
    .await?;
    let (guard, lines) = testkit::logs::recorded();
    PostgresMemory::connect(&url(&pool))
        .with_current_subscriber()
        .await?;
    drop(guard);
    let lines: Vec<String> = lines
        .lock()
        .iter()
        .filter(|line| line.starts_with("WARN "))
        .cloned()
        .collect();
    let [line] = lines.as_slice() else {
        return Err(format!("exactly one rolling-update warning, got {lines:?}").into());
    };
    assert!(
        line.contains(&format!(
            "the memory schema reports version 9999, this build requires {SCHEMA_VERSION} — rolling updates migrate first; continuing"
        )),
        "the warning must name the rolling update: {line}"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_failed_newer_row_is_not_a_schema_version(pool: PgPool) -> TestResult {
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, execution_time, checksum)
         VALUES (9999, 'failed-newer', false, 0, '\\x'::bytea)",
    )
    .execute(&pool)
    .await?;
    PostgresMemory::connect(&url(&pool)).await?;
    Ok(())
}

fn as_role(pool: &PgPool, role: &str) -> String {
    pool.connect_options()
        .as_ref()
        .clone()
        .username(role)
        .password(role)
        .to_url_lossy()
        .to_string()
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_serving_login_that_bypasses_row_security_is_refused(pool: PgPool) -> TestResult {
    for (role, attributes) in [
        ("rh_serving_plain", "NOSUPERUSER NOBYPASSRLS"),
        ("rh_serving_bypass", "NOSUPERUSER BYPASSRLS"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DO $$ BEGIN CREATE ROLE {role} LOGIN {attributes} PASSWORD '{role}'; \
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
        )))
        .execute(&pool)
        .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "GRANT SELECT ON _sqlx_migrations TO {role}"
        )))
        .execute(&pool)
        .await?;
    }
    let pooling = memory_postgres::Pooling::OnDemand;
    for (url, refused) in [
        (url(&pool), true),
        (as_role(&pool, "rh_serving_bypass"), true),
        (as_role(&pool, "rh_serving_plain"), false),
    ] {
        match PostgresMemory::connect_deferred(&url, pooling).await {
            Err(error @ Error::BypassingLogin { .. }) => {
                assert!(refused, "{error}");
                assert!(
                    error
                        .to_string()
                        .contains("serve memory with an ordinary login"),
                    "{error}"
                );
                assert!(!error.to_string().contains("password"), "{error}");
            }
            Ok(_) => assert!(!refused, "a bypassing login was accepted"),
            Err(other) => return Err(other.into()),
        }
    }
    PostgresMemory::connect(&url(&pool)).await?;
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn memory_transfer_requires_postgres_migrations_through_84(pool: PgPool) -> TestResult {
    let through_82 = migrate::POSTGRES
        .migrations
        .partition_point(|migration| migration.version <= 82);
    let legacy = migrate::POSTGRES
        .migrations
        .get(..through_82)
        .ok_or("missing migrations through 82")?;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Borrowed(legacy),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before.run(&pool).await?;
    match PostgresMemory::connect(&url(&pool)).await {
        Err(Error::SchemaBehind { found, required }) => assert_eq!((found, required), (82, 84)),
        other => panic!("expected SchemaBehind, got {other:?}"),
    }
    let versions: Vec<i64> = migrate::POSTGRES
        .migrations
        .iter()
        .filter(|migration| migration.version > 82)
        .map(|migration| migration.version)
        .collect();
    assert_eq!(versions, vec![83, 84]);
    let stats = migrate::postgres_pool(&pool).await?;
    assert_eq!((stats.current, stats.pending), (84, 0));
    PostgresMemory::connect(&url(&pool)).await?;
    Ok(())
}
